---------------------------- MODULE Kernel ----------------------------
\* aterm buffer-kernel CONSISTENCY family (ATERM_DESIGN §3.4, §4).
\*
\* Models the append-only event log that is the kernel's change spine: a
\* monotonic sequence number `seq`, and the log of emitted sequence numbers.
\* Every observable mutation is exactly one append that bumps `seq` by one.
\* This is the smallest real invariant of the kernel: the event log is a
\* gap-free, strictly-increasing record (no torn/duplicated/dropped seq).
\*
\* Bounded by CONSTANT MaxSeq so the state space is finite (TLC/ty checkable).

EXTENDS Naturals, Sequences

CONSTANT MaxSeq

VARIABLES
    seq,   \* the kernel's monotonic event sequence number
    log    \* << seq_1, seq_2, ... >> the appended sequence numbers

vars == << seq, log >>

Init ==
    /\ seq = 0
    /\ log = << >>

\* One observable mutation: append an event, bumping seq by exactly one.
Push ==
    /\ seq < MaxSeq
    /\ seq' = seq + 1
    /\ log' = Append(log, seq')

\* Stutter (a turn in which nothing observable happened) is supplied by the
\* [][Next]_vars subscript below -- NOT as an explicit `\/ UNCHANGED vars`
\* disjunct in Next, which collapses exhaustive search to a single state.
Next == Push

Spec == Init /\ [][Next]_vars

------------------------------------------------------------------------
\* INVARIANTS (the properties ty must prove hold in every reachable state)

\* Types stay in range.
TypeOK ==
    /\ seq \in 0..MaxSeq
    /\ Len(log) \in 0..MaxSeq

\* The spine is gap-free: seq is exactly the number of events appended.
SeqIsLen == seq = Len(log)

\* The log is strictly increasing with no gaps: log[i] = i.
Monotonic == \A i \in 1..Len(log) : log[i] = i
========================================================================
