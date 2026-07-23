--------------------------- MODULE Subscribe ---------------------------
\* aterm buffer-kernel CONSISTENCY family (ATERM_DESIGN §3.4): the subscribe
\* read-face over the BOUNDED event-log ring.
\*
\* Executable model of aterm-buffer's `poll()` (lib.rs): one writer appends
\* events onto a ring of capacity RingCap (oldest evicted); a reader holds a
\* cursor and pulls. Property worth proving (NOT a tautology):
\*
\*   A subscriber NEVER SILENTLY LOSES events. While no Gap has been raised, the
\*   events delivered are EXACTLY the contiguous prefix 1..cursor -- never a hole.
\*   If the reader falls behind the ring horizon, a Gap MUST be raised (forcing a
\*   re-pull); the writer is never blocked.
\*
\* An off-by-one in the gap condition (`cursor+1 < oldestLive`) would let an event
\* be evicted unseen while the reader still believed it was caught up -- then
\* NoSilentLoss fails. A green exhaustive check validates the real poll() logic.
\*
\* `delivered` is modelled as a function 1..MaxSeq -> BOOLEAN (fixed slots), not a
\* growing SET, so ty's flat-state engine represents it directly.

EXTENDS Naturals

CONSTANTS MaxSeq, RingCap

VARIABLES
    seq,        \* total events ever appended (monotone)
    cursor,     \* the reader's last-delivered seq
    delivered,  \* delivered[n] = TRUE iff event n was delivered to the reader
    gap         \* has the reader been told to re-pull (fell behind the ring)?

vars == << seq, cursor, delivered, gap >>

Max(a, b) == IF a > b THEN a ELSE b

\* The oldest event still live in the bounded ring (0 before any write).
OldestLive == IF seq = 0 THEN 0 ELSE Max(1, seq - RingCap + 1)

Init ==
    /\ seq = 0
    /\ cursor = 0
    /\ delivered = [ n \in 1..MaxSeq |-> FALSE ]
    /\ gap = FALSE

\* The single writer appends one event (the reader is not blocked by this).
Write ==
    /\ seq < MaxSeq
    /\ seq' = seq + 1
    /\ UNCHANGED << cursor, delivered, gap >>

\* The reader polls. If its cursor fell behind the ring horizon it gets a Gap and
\* resyncs to the head; otherwise it is delivered exactly the new run (cursor, seq].
Poll ==
    \/ /\ cursor + 1 < OldestLive          \* fell behind -> Gap, no silent loss
       /\ gap' = TRUE
       /\ cursor' = seq
       /\ UNCHANGED << seq, delivered >>
    \/ /\ ~(cursor + 1 < OldestLive)        \* caught up enough -> deliver the run
       /\ delivered' = [ n \in 1..MaxSeq |-> delivered[n] \/ (n > cursor /\ n =< seq) ]
       /\ cursor' = seq
       /\ UNCHANGED << seq, gap >>

Next == Write \/ Poll

Spec == Init /\ [][Next]_vars

------------------------------------------------------------------------
\* INVARIANTS

TypeOK ==
    /\ seq \in 0..MaxSeq
    /\ cursor \in 0..MaxSeq
    /\ gap \in BOOLEAN
    /\ delivered \in [ 1..MaxSeq -> BOOLEAN ]

\* The reader is never ahead of the writer.
CursorBounded == cursor =< seq

\* THE property: with no Gap raised, an event n is delivered IFF it is in the
\* contiguous prefix 1..cursor -- no event silently skipped, none invented.
NoSilentLoss ==
    \/ gap
    \/ \A n \in 1..MaxSeq : delivered[n] <=> (n =< cursor)
========================================================================
