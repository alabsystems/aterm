---------------------------- MODULE FdLifecycle ----------------------------
\* aterm session fd-lifecycle SAFETY family (initiative A7, WS-G/concurrency).
\* Executable model of the PTY-master ownership discipline in
\* `aterm-session/src/sink.rs` (`SinkWriter`):
\*
\*   pub struct SinkWriter {
\*       master: i32,                 \* RAW fd, used for write/read/resize (sink.rs:48)
\*       _owned: Option<OwnedFd>,     \* ownership token (sink.rs:53)
\*   }
\*   \* "A sink built with new_owned OWNS the master fd: it is closed exactly when
\*   \*  the LAST Arc<SinkWriter> clone drops (via the held OwnedFd) -- never by an
\*   \*  out-of-band close()." (sink.rs:32-39)
\*
\* Every party that uses the master (GUI writer, reader thread, resize) holds an
\* `Arc<SinkWriter>` clone and uses the RAW fd `master` for read/resize, "valid
\* for exactly as long as a clone is alive" (sink.rs:39). The FIX ties the fd's
\* close to OwnedFd::drop on the LAST clone, so the raw fd can never be used after
\* it is closed. The pre-fix bare-`i32` master could be `close()`d out-of-band
\* while other clones still held and used the raw number -> use-after-close
\* (a misrouted read/write onto a recycled fd).
\*
\* Modeled (k = MaxClones live clones) as: `clones` (live Arc count), `fdOpen`
\* (is the master fd still open?), and a latched `usedAfterClose` (did any holder
\* read/write/resize the raw fd while it was closed?). Properties:
\*
\*   - NoUseAfterClose:     no holder ever uses the raw master fd after it closed.
\*   - ClosedImpliesNoClones: fd closed => no live clone holds it (strong = 0) --
\*     i.e. the fd outlives every user (the OwnedFd-last-drop guarantee).
\*
\* The fix closes the fd ONLY when the last clone drops, so a closed fd has no
\* live holder and no use can follow. The bug (Buggy) enables an out-of-band
\* close() while clones>0, which makes both invariants reachable-false.

EXTENDS Naturals

CONSTANTS Buggy,        \* TRUE models the pre-fix bare-i32 out-of-band close()
          MaxClones     \* bound on concurrent Arc<SinkWriter> clones (k)

VARIABLES
    clones,         \* number of live Arc<SinkWriter> clones holding the raw fd
    fdOpen,         \* is the PTY master fd still open?
    usedAfterClose  \* latched: did a holder use the raw fd after it was closed?

vars == << clones, fdOpen, usedAfterClose >>

Init ==
    /\ clones = 1            \* new_owned: the original owner holds the OwnedFd
    /\ fdOpen = TRUE
    /\ usedAfterClose = FALSE

\* Arc::clone: another party takes a clone (GUI writer, reader, resize handle).
\* Only possible while a live clone exists (you clone an existing Arc).
Clone ==
    /\ clones > 0
    /\ clones < MaxClones
    /\ clones' = clones + 1
    /\ UNCHANGED << fdOpen, usedAfterClose >>

\* A holder uses the RAW master fd (write_frame / read / resize via `master`).
\* If the fd is open this is sound; if it is already closed it is a
\* use-after-close (a read/write onto a closed/recycled fd) -- latched.
UseFd ==
    /\ clones > 0
    /\ usedAfterClose' = (usedAfterClose \/ ~fdOpen)
    /\ UNCHANGED << clones, fdOpen >>

\* Drop one Arc<SinkWriter> clone. THE FIX: the fd is closed (via OwnedFd::drop)
\* EXACTLY when the last clone drops -- so fdOpen' is "some clone still alive".
DropClone ==
    /\ clones > 0
    /\ clones' = clones - 1
    /\ fdOpen'  = (clones' > 0)      \* close iff this was the last holder
    /\ UNCHANGED usedAfterClose

\* THE DEFECT (pre-fix bare i32 master): an out-of-band close() of the raw fd
\* while other clones are still alive and may still use it. The OwnedFd fix makes
\* this unrepresentable -- only OwnedFd::drop on the last clone closes the fd.
OutOfBandClose ==
    /\ Buggy
    /\ fdOpen
    /\ clones > 0                    \* clones still hold the fd ...
    /\ fdOpen' = FALSE               \* ... yet it is closed out from under them
    /\ UNCHANGED << clones, usedAfterClose >>

Next == Clone \/ UseFd \/ DropClone \/ OutOfBandClose

Spec == Init /\ [][Next]_vars

------------------------------------------------------------------------
\* INVARIANTS

TypeOK ==
    /\ clones \in 0..MaxClones
    /\ fdOpen \in BOOLEAN
    /\ usedAfterClose \in BOOLEAN

\* No party ever uses the raw master fd after it has been closed. In the fix the
\* fd closes only on the last clone's drop, so no live holder can witness a
\* closed fd -- a use-after-close is impossible.
NoUseAfterClose == ~usedAfterClose

\* The fd is closed only when it has no live holder (strong count 0): the fd
\* outlives every user. This is the OwnedFd-last-drop guarantee, and exactly the
\* doc's `fd_state = closed => strong = 0`.
ClosedImpliesNoClones == (~fdOpen) => (clones = 0)

=============================================================================
