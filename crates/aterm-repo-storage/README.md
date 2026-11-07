# Repository storage observations

`aterm machines storage plan --git-common-dir ABS [--json] [--verify-io-bytes N]`
is a read-only filesystem planner. It neither invokes Git, reads credentials,
nor opens network clients. Its library and CLI have no mutation operation.
Filesystem access can still trigger OS-managed remote or cloud-materialization
I/O; this engine does not enforce a no-materialization filesystem policy.

The explicit root must be an absolute real directory reached without following
symlinks, with a bounded regular `HEAD` and real `objects` and `refs` directories. This is
structural admission, not validation of Git objects. The planner observes primary
`objects/pack`, nested stores under `modules`, and module stores under
`worktrees/*/modules`. It never follows a checkout's files. Store namespaces and
pack-directory membership are bounded and rechecked at the end. Files ending in
`.pack` require the exact lowercase `pack-<40 or 64 hex>.pack` spelling.

Equal names and lengths form candidates; distinct `(device,inode)` identities
are counted once, with hardlink aliases reported separately. Every filename
group retains its own distinct-inode path witnesses, so a differently named
alias cannot hide a candidate. Overlapping groups form inode components:
duplicate logical bytes count each inode beyond one per component, rather than
summing repeated path pairs. Verified totals use only byte-equal edges surviving
final validation. Path-pair evidence and actual I/O remain separate counts.
By default the
verification I/O budget is zero and **no file contents are read or hashed**.
An explicit positive budget admits complete paired streaming comparison plus
SHA-256, reserving both lengths plus one EOF lookahead byte per side. Reported
I/O counts actual bytes read from both sides. Insufficient budget defers the
pair before reading. A byte mismatch is distinct from a failed observation.

All directory and file opens are anchored and no-follow. Regular files use
nonblocking opens, so a FIFO cannot wait for a writer. Identity, type, owner,
permissions, length and nanosecond mutation timestamps are checked before and
after comparison; directory membership and file stamps are checked again at the
end. These are **sequential observations**, not an atomic snapshot or exclusion
of future writers. The monotonic elapsed limit is cooperative; it cannot cancel
a blocked filesystem call. Other platforms explicitly refuse until the same
strict boundary is implemented. Bounds and incomplete observations are visible
in the report. Absolute-root admission is capped at 128 lexical components and
4096 path bytes before opening its first component, and shares the same
cooperative elapsed endpoint as the census and final checks.

Candidate and byte-equal bytes are logical file counts. Physical savings remain
unknown because APFS extents may already be shared. Neither byte equality nor
this plan proves valid Git objects, Git reachability, writer quiescence, cleanup
authority or recoverable mutation. Even a completely byte-equal plan always
reports mutation unavailable: real writer custody and durable replacement and
recovery must precede any future CoW operation. No alternates, links, repacking,
deletion or replacement are performed.

Within each filename/length group, verification compares the first distinct
inode against each remainder. It does not compute all equality partitions: when
the first differs from two equal later files, that later equality is not tested.
