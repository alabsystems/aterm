----------------------------- MODULE Evict -----------------------------
\* aterm buffer-kernel CONSISTENCY family (ATERM_DESIGN §3.4): the BOUNDED
\* event-log ring's eviction discipline + poll() gap detection.
\*
\* Executable model of aterm-buffer's `EventLog::append` and `poll()` (lib.rs):
\*
\*   append: ring.push_back(ev); if ring.len() > MAX_LOG_EVENTS { pop_front(); }
\*   poll:   if oldest_live.seq > cursor + 1 { Gap, resync to head }
\*           else deliver the live events with seq > cursor
\*
\* MAX_LOG_EVENTS (1 << 16 in code) is CONSTANT Cap here. The live ring is an
\* explicit function live : [1..MaxSeq -> BOOLEAN] updated OPERATIONALLY
\* (mark the new event live; evict exactly the single oldest live event when
\* over cap), and the invariants check it against the derived ideal window --
\* so an eviction bug (wrong index popped, two popped, none popped) is caught.
\* Properties worth proving:
\*
\*   - LenBounded:    the ring NEVER holds more than Cap events.
\*   - EvictOldestContiguous: the live region is EXACTLY [lo, seq] -- what got
\*     evicted is precisely the oldest contiguous prefix 1..lo-1, never a hole.
\*   - NoSilentLoss:  while no Gap was raised, delivery is exactly the
\*     contiguous prefix 1..cursor (a cursor that fell into the evicted range
\*     MUST surface as a Gap, never as silently skipped events).
\*   - GapJustified:  a Gap is raised ONLY when some event was truly evicted
\*     before delivery (no spurious gap: the code's `oldest > cursor + 1` is
\*     exact -- cursor = oldest-1 still delivers losslessly).
\*
\* live/delivered are functions to BOOLEAN (fixed slots), not growing SETs, so
\* ty's flat-state engine represents them directly.

EXTENDS Naturals

CONSTANTS MaxSeq, Cap

VARIABLES
    seq,        \* total events ever appended (monotone; head of the spine)
    lo,         \* seq of the oldest still-live event (ring head); 1 when empty
    live,       \* live[n] = TRUE iff event n is still in the bounded ring
    cursor,     \* the subscriber's last-delivered seq
    delivered,  \* delivered[n] = TRUE iff event n was delivered to the reader
    gap         \* latched: has a poll ever returned Gap (reader must re-pull)?

vars == << seq, lo, live, cursor, delivered, gap >>

Init ==
    /\ seq = 0
    /\ lo = 1
    /\ live = [ n \in 1..MaxSeq |-> FALSE ]
    /\ cursor = 0
    /\ delivered = [ n \in 1..MaxSeq |-> FALSE ]
    /\ gap = FALSE

\* `EventLog::append`: push the new event, then pop exactly ONE oldest event
\* iff the ring exceeded Cap -- a continuous appender keeps len pinned at Cap.
Append ==
    /\ seq < MaxSeq
    /\ seq' = seq + 1
    /\ IF (seq + 1) - lo + 1 > Cap
       THEN /\ lo' = lo + 1
            /\ live' = [ n \in 1..MaxSeq |->
                           IF n = seq + 1 THEN TRUE ELSE (live[n] /\ n # lo) ]
       ELSE /\ lo' = lo
            /\ live' = [ live EXCEPT ![seq + 1] = TRUE ]
    /\ UNCHANGED << cursor, delivered, gap >>

\* `poll`, gap arm: the ring is non-empty and the oldest live seq is PAST
\* cursor+1 (event cursor+1 was evicted unseen) -> Gap + resync to head.
PollGap ==
    /\ seq > 0
    /\ lo > cursor + 1
    /\ gap' = TRUE
    /\ cursor' = seq
    /\ UNCHANGED << seq, lo, live, delivered >>

\* `poll`, deliver arm: not behind the horizon -> deliver every LIVE event
\* with seq > cursor (the code filters the ring, so only live events flow).
PollDeliver ==
    /\ ~(seq > 0 /\ lo > cursor + 1)
    /\ delivered' = [ n \in 1..MaxSeq |-> delivered[n] \/ (live[n] /\ n > cursor) ]
    /\ cursor' = seq
    /\ UNCHANGED << seq, lo, live, gap >>

Next == Append \/ PollGap \/ PollDeliver

Spec == Init /\ [][Next]_vars

------------------------------------------------------------------------
\* INVARIANTS

TypeOK ==
    /\ seq \in 0..MaxSeq
    /\ lo \in 1..MaxSeq
    /\ cursor \in 0..MaxSeq
    /\ live \in [ 1..MaxSeq -> BOOLEAN ]
    /\ delivered \in [ 1..MaxSeq -> BOOLEAN ]
    /\ gap \in BOOLEAN

\* The bounded ring NEVER exceeds Cap (MAX_LOG_EVENTS), and never goes negative.
LenBounded == seq - lo + 1 =< Cap /\ lo =< seq + 1

\* Eviction takes EXACTLY the oldest contiguous prefix: the operationally
\* maintained ring equals the ideal window [lo, seq] -- live events are
\* suffix-contiguous, the evicted region 1..lo-1 has no survivor and no hole.
EvictOldestContiguous ==
    \A n \in 1..MaxSeq : live[n] <=> (lo =< n /\ n =< seq)

\* The reader is never ahead of the writer.
CursorBounded == cursor =< seq

\* While no Gap was raised, event n was delivered IFF n is in the contiguous
\* prefix 1..cursor: a cursor in the evicted range MUST gap, never skip.
NoSilentLoss ==
    \/ gap
    \/ \A n \in 1..MaxSeq : delivered[n] <=> (n =< cursor)

\* A Gap is never spurious: it is raised only when some event was genuinely
\* evicted (n < lo) without ever being delivered.
GapJustified ==
    gap => \E n \in 1..MaxSeq : n < lo /\ ~delivered[n]
========================================================================
