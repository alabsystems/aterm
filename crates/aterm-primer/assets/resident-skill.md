---
name: aterm-resident
description: Work a wake of this computer's resident, the manager seat aterm books, after aterm assigns this session the seat. Use for the resident's due blanks, titles and summaries, faults carried to a verified repair, prevention filed as a package, and addressed fleet mail routed with its original envelope.
---
<!-- aterm skill v1 — MANAGED FILE, rewritten by `aterm agents` on every install/update; remove this marker line and aterm will leave the file alone (reported as `foreign`) -->

# Work a resident wake

Use this procedure when aterm has booked your session as the resident. Other
sessions keep their own assignments. The brief aterm writes into the resident
folder's context file at each start holds the current edition and its ten
rules; a reading's `method` names that edition (`cz@2`).

1. Start with `aterm resident due`: the open blanks first, each with what it
   needs, then what changed since your cursor. Where this build has no
   `aterm resident` verb yet, read the changed subject's claims with
   `aterm ctl @<sid> described` and a bounded part of its log
   (`aterm ctl help log` gives the installed bounds). Carry the full session
   ID through a restart or a title change, and keep a run's identity with the
   evidence that describes it.
2. For a resource question, identify the machine, the sample time, the unit and
   the owning process or session. Missing samples, stderr output, silence and a
   request for input each describe different things. Read the object's own
   definition and freshness before interpreting its value, and keep an
   unavailable reading explicit.
3. For a title, name the work so another reader can recognize it. For a
   summary, say what the session is about, why it matters, what it is doing and
   what changed, citing facts and their boundary. Where a cheaper model is
   enough, ask it with `aterm resident model <id> --with <cli>`; the record
   names the model that answered. Keep uncertainty where the evidence runs out.
4. Fill a blank with `aterm resident fill <id> <value>`. Measured answers have
   no blank: aterm fills them. Never write over a person's statement; keep to a
   person's correction. Before the typed store, publish only changed
   description claims in the session log: for example,
   `aterm ctl @<sid> mark - desc field=title value=Cache%20restore` sets a title
   claim; the other slots are `matters` and `description`. Percent-encode the
   value, respect the log's length and rate bounds, and read `described` again
   to see which claim stands.
5. Route mail addressed to this computer with
   `aterm resident route <message> to=@<sid>`, which keeps the original sender,
   destination and request id and names you as a hop; mail from a session on
   this computer goes on as a note marked `trust=relayed`. Read queued,
   delivered and answered with `aterm resident receipts <message>`; a receipt
   and an answer are separate events, and a second attempt reuses the request
   id. Where this build has no `aterm resident route`, leave the message in
   your inbox and record that it was not routed and why; never retell it in
   your own words as its sender's. A reply you author yourself names you as its
   sender: for a local addressed question,
   `aterm ctl @self post to=@<sid> kind=answer re=<off> -- '<answer>'`.
   A worker's terminal input is outside this route.
6. Carry a fault to a verified repair within the computer's grants, with
   aterm's own verbs: the disk (`aterm ctl @self await sys 'free>=room'`,
   `aterm harness disk --apply <class>`, `aterm pkg machine cache`, which free
   rebuildable output only), what closed sessions left (`aterm ctl sys left`,
   then `aterm ctl end <token>`), aterm's installation (`aterm doctor`,
   `aterm pkg repair`), AI programs and credentials (`aterm ai doctor`,
   `aterm harness accounts`, `aterm secrets sync`, `aterm secrets need <want>`),
   a stalled update (`aterm ctl update retry`), the fabric
   (`aterm fabric doctor`) and the keeper (`aterm keeper start`). Workers
   waiting on a newer build are a fact you read with
   `aterm harness upgrade --status`; their move is their harness's. Record each
   act with its facts and outcome (`aterm resident act`, where this build has
   it, or your own conversation). A step only a person can take is recorded
   once as an incident that needs a person and worked around; run nothing that
   opens a panel or asks a question of whoever is at the computer.
7. When recovery would have needed a person, write the retrospective at once,
   even if nobody intervened. Name the computer, your booked session ID and
   the affected session or operation separately. Cite the failed operation,
   its time and source, and each automatic recovery with its observed outcome.
   Name the component in aterm or aos, the behavior to change, and a test that
   reproduces the original conditions unattended. File that change as a
   package of the lab tracker (`goals/` in the publication repository), link
   it to the incident, and keep the incident open until the package's test
   passes on main. Write and land no code for it yourself: a code change is a
   worker's task. Restoring service and verifying prevention are separate
   events.

Read a whole transcript, log or survey only when a person typing in your own
terminal asks for that scope. A routine wake ends when what is due is written.
