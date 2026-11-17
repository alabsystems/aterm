<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-capacity: specs

> The crate's first rows are DC-23's: a person's claim at a tab goes ahead of the agents' claims of
> its rank. They were written with the code they state, and the tests named in them ran. The
> crate's other tests are not mapped here yet. None of these specs is proved.

Notation: a claim's *mark* is `Need::person`; its *priority* is `Need::priority` =
`(rank, ¬person)`, with `Rank` ordered Release < Ordinary < Background; its *order* is
`Record::order` = `(start_ms, pid, seq)`. A waiter `o` is *ahead of* a waiter `n` where
`(¬o.leads, o.priority, o.order) < (¬n.leads, n.priority, n.order)`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `primary_tests` (how many tests it retires as
primary; the list is in §2).

```tsv
id	kind	functions	statement	primary_tests
person-mark	contract	admit::Waiter::enter_accounted; admit::Waiter::enter_at; admit::Place::standing; admit::Stood::of; ledger::Ledger::lay_record; ledger::Record::decode; reading::Machine::person	A claim's mark is written by admission as it enters the queue, never by its caller: mark = (rank = Ordinary ∧ ¬leads ∧ m), where m is the mark of the place it enters at (Place::standing) when a holder record of this process still stands there in that ledger, else machine.person(). Every record a ledger lays or reads has mark ⇒ rank = Ordinary ∧ ¬leads. A place's mark is the mark of the first record laid at it, and holds while any waiter or claim at it stands; once none does, an entry at it reads the machine. A machine's default person() is false, so an injected machine marks nothing.	1
person-priority	contract	Need::priority; admit::decide_with; admit::reservations_as; admit::memory_turn; admit::reserving; admit::ranked_grant; admit::Waiter::look	Every comparison that orders waiters orders them by (¬leads, priority, order), or by (priority, order) where it read (rank, order) before: decide's ahead, the reservations, the memory turn, a quiet claim's reservation over claimed work, ranked_grant's reserved cores and fair-share parties, and the Now guard (`--admit now` overrides no Release or leading waiter's memory turn). So a marked waiter is ahead of every unmarked waiter of its rank that does not lead, whatever their order, and of nothing else; a held claim is ahead of every waiter; waiters of one (leads, priority) keep arrival order; sure_leads and the order of leads among themselves are unchanged.	2
person-record	contract	ledger::Record::head_line; ledger::Record::decode	A marked record's head is an unmarked one's with the one key `person: true` added; the rank word stays Release, Ordinary or Background. A head without the key, or with any value but true, reads unmarked.	1
person-terminal-rule	contract	reading::person_at; reading::Live::person; reading::terminal_foreground	person_at(fg, g, var) ⇔ g > 0 ∧ fg = Some(g) ∧ no AGENT_MARKERS variable is set and non-empty in var ∧ ATERM_TASK_FILE is unset or empty in var. Live::person() = person_at(tcgetpgrp of /dev/tty opened O_NOCTTY|O_NONBLOCK|O_NOFOLLOW|O_CLOEXEC under kept_out (None where it cannot be opened or answers no group), getpgrp(), the process environment read as OS strings, lossily); false off unix.	2
person-probe	contract	resource_probe::ResourceProbe::capture_at; resource_probe::ResourceProbe::rejection_placed	A probe judges an envelope with the mark admission would give its claim: the mark of the place it is judged at where a record of this process stands there in the ledger the probe read, else machine.person() as read at capture. So a person's envelope is not rejected behind an agent's earlier waiter of its rank, and an envelope judged at an agent's place is.	1
```

## 2. Test → spec map (TSV)

Paths are relative to `crates/aterm-capacity/`. `primary` is the spec that retires the test;
`also` lists other specs the same test samples.

```tsv
file	test	primary	also
src/admit_person_tests.rs	a_persons_claim_at_a_tab_is_admitted_before_agent_claims_that_asked_first	person-priority	person-mark
src/admit_person_tests.rs	release_leads_and_background_order_is_unchanged_beside_a_persons_claim	person-priority	person-mark
src/admit_person_tests.rs	a_claim_entered_at_a_place_keeps_the_places_mark	person-mark	
src/ledger_person_tests.rs	a_persons_record_adds_one_key_and_keeps_every_other	person-record	person-mark
src/admit_person_tests.rs	the_terminal_rule_reads_a_person_only_in_the_foreground_group_with_no_agent_marker	person-terminal-rule	
src/admit_person_tests.rs	the_live_terminal_rule_reads_the_foreground_group_of_dev_tty	person-terminal-rule	
src/resource_probe_tests.rs	a_persons_probe_is_not_blocked_behind_an_agents_earlier_waiter	person-probe	person-mark
```

## 3. Test stand-ins

`standin_reads_live_person` and `standin_leads_a_terminal_and_starts_readers`
(`src/admit_person_tests.rs`) are processes `the_live_terminal_rule_reads_the_foreground_group_of_dev_tty`
starts; run alone, each does nothing. They are not spec samples.
