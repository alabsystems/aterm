----------------------------- MODULE WriteAll -----------------------------
\* aterm I/O DURABILITY family: the write-all loop that drains a buffer to a
\* file descriptor, retrying on EINTR and short writes (fix 3d58709).
\*
\* Executable model of aterm's blocking `write_all` over a byte buffer of
\* length Size. The real defect: the loop used the raw `write()` return value
\* and treated an EINTR (return 0) or a SHORT write (return < remaining) as
\* completion -- reporting success while silently DROPPING the unwritten tail.
\* The fix loops until every byte is written: EINTR retries with the offset
\* UNCHANGED, a short write advances the offset and keeps going, and `done` is
\* latched ONLY when off has reached Size.
\*
\*   off  : 0..Size bytes already written (monotone non-decreasing)
\*   done : has the loop REPORTED completion to its caller?
\*
\* The one property that matters here is the safety of that report:
\*
\*   - NoSilentDrop:  done => off = Size. The loop announces success ONLY when
\*     the whole buffer is flushed -- never abandoning a partial write as if it
\*     had finished. With Buggy = TRUE an EINTR/short write sets done early
\*     (off < Size), which ty reaches and reports as a counterexample.
\*   - OffMonotone is implied by construction; OffBounded keeps off in range.
\*
\* This is the same VC ("the loop only exits when off = Len") proved deductively
\* elsewhere; here ty discharges it as a reachability/state-machine invariant.

EXTENDS Naturals

CONSTANTS Size, Buggy

VARIABLES
    off,    \* bytes successfully written so far (cursor into the buffer)
    done    \* has the write loop reported completion to the caller?

vars == << off, done >>

Init ==
    /\ off = 0
    /\ done = FALSE

\* A normal partial/full progress step: write k in 1..(Size-off) bytes. When
\* this reaches the end of the buffer the loop terminates and reports success.
Progress ==
    /\ ~done
    /\ off < Size
    /\ \E k \in 1..(Size - off) :
         /\ off' = off + k
         /\ done' = (off + k = Size)

\* An interrupted/short syscall that did NOT finish the buffer (EINTR returns 0;
\* a short write leaves bytes remaining). The CORRECT loop keeps going without
\* claiming completion; the BUGGY loop mistakes this for completion (done=TRUE)
\* while the tail off < Len is still unflushed -- the dropped-tail defect.
Interrupted ==
    /\ ~done
    /\ off < Size
    /\ \E k \in 0..(Size - off - 1) :   \* k < remaining, so off+k < Size
         IF Buggy
         THEN /\ off' = off + k
              /\ done' = TRUE          \* BUG: report success with tail unwritten
         ELSE /\ off' = off + k        \* FIX: advance (or stay, on EINTR) ...
              /\ done' = FALSE         \* ... and keep looping, no false success

Next == Progress \/ Interrupted

Spec == Init /\ [][Next]_vars

------------------------------------------------------------------------
\* INVARIANTS

TypeOK ==
    /\ off \in 0..Size
    /\ done \in BOOLEAN

\* The write offset never escapes the buffer.
OffBounded == off =< Size

\* THE property: the loop reports completion ONLY when every byte was written.
\* A `done` with off < Size is a silently dropped tail -- exactly the bug.
NoSilentDrop == done => off = Size
========================================================================
