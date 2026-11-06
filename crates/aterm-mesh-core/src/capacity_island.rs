// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P6, the membership log cannot wedge: the room rule of `capacity.rs` and its laws, proved for
// every size by the kernel on every Trust build. The spec kernels are what capacity.rs's kernels
// compute; the World of section 3 asks the kernels as parameters, and
// `capacity_binding_island.rs` instantiates it with the compiled Rust. `capacity::tests`
// recomputes section 7's answer sheet from the Rust and reads its words from this file.
clean {
    -- The decision's scalar kernels. stop_judges and writer_waits, the contract!
    -- kernels of crates/aterm-mesh-core/src/capacity.rs, state `ensures result ==
    -- spec_<name>(..) by <name>_is_exact`, and the binding island proves each
    -- compiled body equal to the definition here. keeps_room compares a computed
    -- integer, which E6 does not import yet (Trust's I2'), so section 7's sheet
    -- holds the Rust to spec_keeps_room until then; section 1 proves what
    -- spec_keeps_room means at every u64. They sit outside the namespace so a
    -- clause can name them.

    -- spec_keeps_room: a log holding `held` of `limit` takes `cost` more and keeps
    -- `need` free, written as the Rust is, with no sum that can wrap. Its meaning,
    -- `held + cost + need <= limit` over Nat, is section 1's keeps_room_means.
    def spec_keeps_room (held cost need limit : UInt64) : Bool :=
      decide (held ≤ limit) && (decide (cost ≤ limit - held) && decide (need ≤ limit - held - cost))
    -- spec_stop_judges: the 8,500 stop judges a re-key only where no held Remove
    -- made it due.
    def spec_stop_judges (rotation : Bool) : Bool := !rotation
    -- spec_writer_waits: a writer that waits for keys and minted nothing waits while
    -- the audience is due a rotation.
    def spec_writer_waits (waitsForKey mintedHere rotationDue : Bool) : Bool :=
      waitsForKey && (!mintedHere && rotationDue)

    namespace AtermMeshCapacity

    open AtermMeshTemporal

    -- ===========================================================================
    -- 1. What the kernels mean, and what the World asks of them.
    -- ===========================================================================

    -- The room the World reads: the sum over Nat, which never wraps.
    def roomNat (held cost need limit : Nat) : Bool := decide (held + cost + need ≤ limit)

    -- spec_keeps_room means roomNat at every u64: keeps_room_means below.
    def KeepsRoomMeans : Prop :=
      ∀ (h c n l : UInt64), spec_keeps_room h c n l = roomNat h.toNat c.toNat n.toNat l.toNat

    -- Its proof. Clean's core has no lemma for UInt64 subtraction's Nat meaning
    -- and its omega reads no truncated subtraction, so the few Nat facts it needs
    -- are proved here from Nat.div_add_mod, Nat.mod_lt and the successor cases:
    -- `(l - h).toNat` is `(l.toNat + (2^64 - h.toNat)) % 2^64` by definition, which
    -- is `l.toNat - h.toNat` once `h <= l` (kr_toNat_sub), and each subtraction of
    -- spec_keeps_room is read only past the comparison that keeps it from wrapping.
    theorem kr_sub_add_cancel (m : Nat) : (n : Nat) → m ≤ n → n - m + m = n := by
      induction m with
      | zero => intro n _; rfl
      | succ m ih =>
        intro n h
        cases n with
        | zero => exact False.elim (Nat.not_succ_le_zero m h)
        | succ n =>
          rw [Nat.succ_sub_succ n m]
          show Nat.succ (n - m + m) = Nat.succ n
          rw [ih n (Nat.le_of_succ_le_succ m n h)]

    theorem kr_le_sub_of_add_le (a b c : Nat) (h : a + b ≤ c) : a ≤ c - b := by
      have h2 := Nat.sub_le_sub_right (a + b) c b h
      rw [Nat.add_sub_cancel a b] at h2
      exact h2

    theorem kr_add_le_of_le_sub (a b c : Nat) (hb : b ≤ c) (h : a ≤ c - b) : a + b ≤ c := by
      have h2 := Nat.add_le_add_right a (c - b) h b
      rw [kr_sub_add_cancel b c hb] at h2
      exact h2

    theorem kr_le_rw (a b c : Nat) (h : a ≤ b) (e : b = c) : a ≤ c := by
      rw [← e]
      exact h

    theorem kr_lin2 (t S m r : Nat) (h : S + t + m = r + S) (ht : S ≤ t) (hr : r < S) : False :=
      have h1 : (t + m) + S = r + S :=
        Eq.trans (Nat.add_comm (t + m) S) (Eq.trans (Nat.add_assoc S t m).symm h)
      have h2 : t + m = r := Nat.add_right_cancel (t + m) S r h1
      have h3 : t ≤ r := kr_le_rw t (t + m) r (Nat.le_add_right t m) h2
      Nat.lt_irrefl r (Nat.lt_of_lt_of_le r S r hr (Nat.le_trans S t r ht h3))

    theorem kr_mod_unique (q m r S : Nat) (h : q * S + m = r + S) (hm : m < S) (hr : r < S) : m = r := by
      cases q with
      | zero =>
        have h0 : m = r + S :=
          Eq.trans (Eq.trans (Nat.zero_add m).symm (congrArg (fun x => x + m) (Nat.zero_mul S)).symm) h
        have hSm : S ≤ m := kr_le_rw S (r + S) m (Nat.le_add_left S r) h0.symm
        exact False.elim (Nat.lt_irrefl m (Nat.lt_of_lt_of_le m S m hm hSm))
      | succ q =>
        have h1 : S + q * S + m = r + S :=
          Eq.trans (congrArg (fun x => x + m) (Nat.succ_mul q S)).symm h
        cases q with
        | zero =>
          have h2 : S + m = S + r :=
            Eq.trans (Eq.trans (congrArg (fun x => x + m) (Eq.trans (congrArg (fun x => S + x) (Nat.zero_mul S)) (Nat.add_zero S))).symm h1) (Nat.add_comm r S)
          exact Nat.add_left_cancel S m r h2
        | succ q =>
          have ht : S ≤ Nat.succ q * S :=
            kr_le_rw S (S + q * S) (Nat.succ q * S) (Nat.le_add_right S (q * S)) (Nat.succ_mul q S).symm
          exact False.elim (kr_lin2 (Nat.succ q * S) S m r h1 ht hr)

    theorem kr_add_mod_self (r S : Nat) (hr : r < S) : (r + S) % S = r :=
      have hS : 0 < S := Nat.lt_of_le_of_lt 0 r S (Nat.zero_le r) hr
      kr_mod_unique ((r + S) / S) ((r + S) % S) r S (Nat.div_add_mod (r + S) S) (Nat.mod_lt (r + S) S hS) hr

    theorem kr_lin3 (x a b y S : Nat) (ha : a + y = x) (hb : b + y = S) : x + b = a + S := by
      rw [← ha, ← hb]
      rw [Nat.add_assoc a y b, Nat.add_comm y b]

    theorem kr_sub_mod (x y S : Nat) (hy : y ≤ x) (hx : x < S) : (x + (S - y)) % S = x - y :=
      have hyS : y ≤ S := Nat.le_of_lt y S (Nat.lt_of_le_of_lt y x S hy hx)
      have e : x + (S - y) = (x - y) + S := kr_lin3 x (x - y) (S - y) y S (kr_sub_add_cancel y x hy) (kr_sub_add_cancel y S hyS)
      have hlt : x - y < S := Nat.lt_of_le_of_lt (x - y) x S (Nat.sub_le x y) hx
      Eq.trans (congrArg (fun z => z % S) e) (kr_add_mod_self (x - y) S hlt)

    theorem kr_toNat_sub_def (a b : UInt64) : (a - b).toNat = (a.toNat + (UInt64.size - b.toNat)) % UInt64.size := rfl

    theorem kr_toNat_sub (a b : UInt64) (h : b.toNat ≤ a.toNat) : (a - b).toNat = a.toNat - b.toNat :=
      Eq.trans (kr_toNat_sub_def a b) (kr_sub_mod a.toNat b.toNat UInt64.size h (UInt64.toNat_lt a))

    theorem kr_decide_true (p : Prop) [inst : Decidable p] (hp : p) : decide p = true :=
      match inst with
      | isTrue _ => rfl
      | isFalse hn => absurd hp hn

    theorem kr_decide_false (p : Prop) [inst : Decidable p] (hp : ¬p) : decide p = false :=
      match inst with
      | isTrue hy => absurd hy hp
      | isFalse _ => rfl

    theorem kr_rev3 (a b c : Nat) : a + b + c = c + b + a := by
      rw [Nat.add_comm a b, Nat.add_comm (b + a) c, ← Nat.add_assoc c b a]

    theorem kr_le_rw_l (a b c : Nat) (h : b ≤ c) (e : a = b) : a ≤ c := by
      rw [e]
      exact h

    theorem kr_room_of (H C N L : Nat) (h1 : H ≤ L) (h2 : C ≤ L - H) (h3 : N ≤ L - H - C) :
        H + C + N ≤ L :=
      kr_le_rw_l (H + C + N) (N + C + H) L
        (kr_add_le_of_le_sub (N + C) H L h1 (kr_add_le_of_le_sub N C (L - H) h2 h3)) (kr_rev3 H C N)

    theorem kr_room_split (H C N L : Nat) (hr : H + C + N ≤ L) : N + C ≤ L - H :=
      kr_le_sub_of_add_le (N + C) H L (kr_le_rw_l (N + C + H) (H + C + N) L hr (kr_rev3 H C N).symm)

    theorem kr_room_n (H C N L : Nat) (hr : H + C + N ≤ L) : N ≤ L - H - C :=
      kr_le_sub_of_add_le N C (L - H) (kr_room_split H C N L hr)

    theorem kr_room_c (H C N L : Nat) (hr : H + C + N ≤ L) : C ≤ L - H :=
      Nat.le_trans C (N + C) (L - H) (Nat.le_add_left C N) (kr_room_split H C N L hr)

    theorem kr_room_h (H C N L : Nat) (hr : H + C + N ≤ L) : H ≤ L :=
      Nat.le_trans H (H + C + N) L
        (Nat.le_trans H (H + C) (H + C + N) (Nat.le_add_right H C) (Nat.le_add_right (H + C) N)) hr

    theorem kr_means_at (h c n l : UInt64) :
        spec_keeps_room h c n l = roomNat h.toNat c.toNat n.toNat l.toNat := by
      show (decide (h ≤ l) && (decide (c ≤ l - h) && decide (n ≤ l - h - c))) =
        decide (h.toNat + c.toNat + n.toNat ≤ l.toNat)
      by_cases h1 : h.toNat ≤ l.toNat
      · have e1 : (l - h).toNat = l.toNat - h.toNat := kr_toNat_sub l h h1
        rw [kr_decide_true (h ≤ l) h1]
        by_cases h2 : c.toNat ≤ l.toNat - h.toNat
        · have h2' : c.toNat ≤ (l - h).toNat := kr_le_rw c.toNat (l.toNat - h.toNat) (l - h).toNat h2 e1.symm
          have e2 : (l - h - c).toNat = l.toNat - h.toNat - c.toNat :=
            Eq.trans (kr_toNat_sub (l - h) c h2') (congrArg (fun x => x - c.toNat) e1)
          rw [kr_decide_true (c ≤ l - h) h2']
          by_cases h3 : n.toNat ≤ l.toNat - h.toNat - c.toNat
          · have h3' : n.toNat ≤ (l - h - c).toNat :=
              kr_le_rw n.toNat (l.toNat - h.toNat - c.toNat) (l - h - c).toNat h3 e2.symm
            rw [kr_decide_true (n ≤ l - h - c) h3',
              kr_decide_true (h.toNat + c.toNat + n.toNat ≤ l.toNat) (kr_room_of h.toNat c.toNat n.toNat l.toNat h1 h2 h3)]
          · have h3' : ¬ (n.toNat ≤ (l - h - c).toNat) := fun hn =>
              h3 (kr_le_rw n.toNat (l - h - c).toNat (l.toNat - h.toNat - c.toNat) hn e2)
            rw [kr_decide_false (n ≤ l - h - c) h3',
              kr_decide_false (h.toNat + c.toNat + n.toNat ≤ l.toNat) (fun hr => h3 (kr_room_n h.toNat c.toNat n.toNat l.toNat hr))]
        · have h2' : ¬ (c.toNat ≤ (l - h).toNat) := fun hc =>
            h2 (kr_le_rw c.toNat (l - h).toNat (l.toNat - h.toNat) hc e1)
          rw [kr_decide_false (c ≤ l - h) h2',
            kr_decide_false (h.toNat + c.toNat + n.toNat ≤ l.toNat) (fun hr => h2 (kr_room_c h.toNat c.toNat n.toNat l.toNat hr))]
      · rw [kr_decide_false (h ≤ l) h1,
          kr_decide_false (h.toNat + c.toNat + n.toNat ≤ l.toNat) (fun hr => h1 (kr_room_h h.toNat c.toNat n.toNat l.toNat hr))]

    theorem keeps_room_means : KeepsRoomMeans := kr_means_at

    -- Exactness: what a kernel the World asks must compute. The binding island
    -- proves it of the compiled stop and wait.
    def RoomExact (room : Nat → Nat → Nat → Nat → Bool) : Prop :=
      ∀ h c n l, room h c n l = roomNat h c n l
    def JudgesExact (judges : Bool → Bool) : Prop := ∀ r, judges r = spec_stop_judges r
    def WaitsExact (waits : Bool → Bool → Bool → Bool) : Prop :=
      ∀ k m d, waits k m d = spec_writer_waits k m d

    theorem room_true (room : Nat → Nat → Nat → Nat → Bool) (hR : RoomExact room) (h c n l : Nat)
        (hok : room h c n l = true) : h + c + n ≤ l :=
      of_decide_eq_true (Eq.trans (hR h c n l).symm hok)

    theorem and_right (a b : Bool) (h : (a && b) = true) : b = true :=
      (Eq.mp (Bool.and_eq_true a b) h).2

    -- A writer waits only on a re-key the stop never judges: both read one
    -- rotation-due fact (`wraps_removed`).
    theorem writer_waits_only_on_exempt (judges : Bool → Bool) (hJ : JudgesExact judges)
        (waits : Bool → Bool → Bool → Bool) (hW : WaitsExact waits)
        (k m due : Bool) (h : waits k m due = true) : judges due = false := by
      rw [hJ due]
      rw [hW k m due] at h
      cases due with
      | true => rfl
      | false => cases k <;> cases m <;> exact Bool.noConfusion h

    -- Plain-variable arithmetic (Clean's omega reads no structure projection).
    theorem ar_reserve (k c a n L : Nat) (h1 : c + a ≤ n) (h2 : k + n ≤ L) : k + c + a ≤ L := by
      omega
    theorem ar_fits (k c a n L : Nat) (h1 : c + a ≤ n) (h2 : k + n ≤ L) : k + c ≤ L := by omega
    theorem ar_auto (k r c a n L : Nat) (h : k + r + c + n ≤ L) (ha : a = n) : k + c + a ≤ L := by
      omega
    theorem ar_init (k n L : Nat) (h1 : k = 0) (h2 : n = 0) : k + n ≤ L := by omega
    theorem ar_prune (k j n L : Nat) (hj : j ≤ k) (h : k + n ≤ L) : j + n ≤ L := by omega
    theorem ar_remove (k b a x L : Nat) (hc : a ≤ x) (h : k + b + x ≤ L) : k + b + a ≤ L := by
      omega

    -- ===========================================================================
    -- 2. The production constants (budgets.rs, readmit.rs).
    -- ===========================================================================

    def stopOf (keyedBudget : Nat) : Nat := keyedBudget * 8500 / 12288
    def proofLine (keyedBudget : Nat) : Nat := (keyedBudget * 3 + 3) / 4
    theorem stop_production : stopOf 12288 = 8500 := by decide
    theorem proof_line_production : proofLine 12288 = 9216 := by decide
    theorem stop_below_proof_line_production : stopOf 12288 < proofLine 12288 := by decide

    -- ===========================================================================
    -- 3. The World: one Mac's log in one dimension, records or bytes, at every
    --    size.
    -- ===========================================================================

    -- L is the bound (RecordBudgets::keyed, 12,288 records, or held_bytes_bound);
    -- S the tender's stop (8,500 records; the bound itself for bytes, which have
    -- none); KR the proof-admission line (9,216 records; past the bound for bytes).
    structure Budgets where
      L : Nat
      S : Nat
      KR : Nat

    def records : Budgets := Budgets.mk 12288 8500 9216
    def bytesOf (bound : Nat) : Budgets := Budgets.mk bound bound (bound + 1)

    -- keyed: the log's keyed records (Log::counts) or wire bytes (Log::held_bytes);
    -- need: what the re-keys a held Remove made due owe at worst, of the dues some
    -- Mac can sign (log_entries.rs owed_rekeys over owed_audiences: a due no Mac
    -- carries is never signed, so no fill refuses it); stuck: a re-key writers wait
    -- on was refused at a fill that only grows, the wedge.
    structure World where
      keyed : Nat
      need : Nat
      stuck : Bool

    -- One keyed write, its cost and the need after it.
    inductive Event where
      | tend (rotation : Bool) (ready cost after : Nat) -- tend_epochs_at, one due audience, `ready` kept before it
      | write (rotationDue : Bool) (cost after : Nat) -- a person's [everyone] Put
      | prove (cost after : Nat) -- readmit's decide: an Admit and its Wraps
      | heal (cost : Nat) -- close_wrap_gaps: one Wrap
      | person (cost after : Nat) -- any other keyed write a person makes
      | remove (batch worst after : Nat) -- remove_member with its RemovalFill
      | prune (kept : Nat) -- D8: a final checkpoint keeps `kept`

    -- Faithfulness: what the Rust computes about an event, held by tests, not
    -- proved. A due re-key releases at least its own cost from need (rekey_bound is
    -- never below what a re-key signs: remove_tests'
    -- the_planned_cost_of_an_audience_not_opened_whole_counts_no_superseded_write,
    -- record_budgets' a_rekeys_records_take_their_most_bytes); a re-key no removal
    -- made is of an audience no held Remove owes, so it changes no due; a removal's
    -- worst case covers every re-key owed after it (capacity_tests'
    -- a_removal_reserves_the_rekeys_an_earlier_removal_made_due and
    -- a_removal_reserves_its_rekeys_bytes); a checkpoint only drops records.
    def WF (w : World) : Event → Prop
      | Event.tend true _ cost after => cost + after ≤ w.need
      | Event.tend false _ _ after => after = w.need
      | Event.remove _ worst after => after ≤ worst
      | Event.prune kept => kept ≤ w.keyed
      | _ => True

    -- One batch, all or none: signed (`ok`), or refused; a refused due re-key
    -- wedges its writers.
    def sign (w : World) (ok : Bool) (cost after : Nat) (rot : Bool) : World :=
      match ok with
      | true => World.mk (w.keyed + cost) after w.stuck
      | false => World.mk w.keyed w.need (w.stuck || rot)

    -- THE RULE AS SHIPPED, over the kernels it asks: the tender
    -- (within_automatic_stop: the stop, and the owed room, for a re-key the stop
    -- judges), the writer's wait (waits_in), a heal (heal_room) and a removal
    -- (RemovalFill) through room; a re-key a held Remove made due, a proof admission
    -- and a person's other writes through Log::insert's hard bound alone.
    def stepNow (b : Budgets) (judges : Bool → Bool) (waits : Bool → Bool → Bool → Bool)
        (room : Nat → Nat → Nat → Nat → Bool) (w : World) : Event → World
      | Event.tend rot ready cost after =>
          match judges rot with
          | true =>
              sign w (room (w.keyed + ready) cost 0 b.S && room (w.keyed + ready) cost w.need b.L)
                cost after rot
          | false => sign w (decide (w.keyed + cost ≤ b.L)) cost after rot
      | Event.write due cost after =>
          match waits true false due with
          | true => w
          | false => sign w (decide (w.keyed + cost ≤ b.L)) cost after false
      | Event.prove cost after =>
          sign w (decide (w.keyed < b.KR) && decide (w.keyed + cost ≤ b.L)) cost after false
      | Event.heal cost => sign w (room w.keyed cost w.need b.L) cost w.need false
      | Event.person cost after => sign w (decide (w.keyed + cost ≤ b.L)) cost after false
      | Event.remove batch worst after => sign w (room w.keyed batch worst b.L) batch after false
      | Event.prune kept => World.mk kept w.need w.stuck

    -- THE RULE P6 PROPOSES for what the shipped one leaves (F3, F4): a proof
    -- admission and a person's writes keep need free too.
    def stepFix (b : Budgets) (judges : Bool → Bool) (waits : Bool → Bool → Bool → Bool)
        (room : Nat → Nat → Nat → Nat → Bool) (w : World) : Event → World
      | Event.write due cost after =>
          match waits true false due with
          | true => w
          | false => sign w (room w.keyed cost after b.L) cost after false
      | Event.prove cost after =>
          sign w (decide (w.keyed < b.KR) && room w.keyed cost after b.L) cost after false
      | Event.person cost after => sign w (room w.keyed cost after b.L) cost after false
      | e => stepNow b judges waits room w e

    def Inv (b : Budgets) (w : World) : Prop := w.keyed + w.need ≤ b.L ∧ w.stuck = false

    -- ===========================================================================
    -- 4. The shipped rule never wedges under the premises it leaves.
    -- ===========================================================================

    -- The stop holds no due re-key: its step reads no S, at any fill.
    theorem tend_exempt (b : Budgets) (judges : Bool → Bool) (hJ : judges true = false)
        (waits : Bool → Bool → Bool → Bool) (room : Nat → Nat → Nat → Nat → Bool)
        (w : World) (ready cost after : Nat) :
        stepNow b judges waits room w (Event.tend true ready cost after) =
          sign w (decide (w.keyed + cost ≤ b.L)) cost after true := by
      simp only [stepNow, hJ]

    theorem sign_inv (b : Budgets) (w : World) (ok : Bool) (cost after : Nat) (rot : Bool)
        (hi : Inv b w) (hok : ok = true → w.keyed + cost + after ≤ b.L)
        (hrot : ok = false → rot = false) : Inv b (sign w ok cost after rot) := by
      cases ok with
      | true => exact And.intro (hok rfl) hi.2
      | false =>
        have hr : rot = false := hrot rfl
        exact And.intro hi.1 (by show (w.stuck || rot) = false; rw [hi.2, hr])

    -- THE PREMISES THE SHIPPED RULE LEAVES (Calm), each in records and in bytes: a
    -- person's [everyone] Put and other writes (F4) and a proof admission (F3) keep
    -- need free, which they do not check. F1, F2, F6 and a heal's F4 need none.
    def Calm (b : Budgets) (w : World) : Event → Prop
      | Event.write _ cost after => w.keyed + cost + after ≤ b.L
      | Event.prove cost after => w.keyed + cost + after ≤ b.L
      | Event.person cost after => w.keyed + cost + after ≤ b.L
      | _ => True

    theorem now_preserves_when_calm (b : Budgets) (judges : Bool → Bool) (hJ : JudgesExact judges)
        (waits : Bool → Bool → Bool → Bool) (room : Nat → Nat → Nat → Nat → Bool)
        (hR : RoomExact room) (w : World) (e : Event) (hwf : WF w e) (hc : Calm b w e)
        (hi : Inv b w) : Inv b (stepNow b judges waits room w e) := by
      have hJt : judges true = false := hJ true
      have hJf : judges false = true := hJ false
      cases e with
      | tend rot ready cost after =>
        cases rot with
        | true =>
          rw [tend_exempt b judges hJt waits room w ready cost after]
          have hw : cost + after ≤ w.need := hwf
          exact sign_inv b w _ cost after true hi
            (fun _ => ar_reserve w.keyed cost after w.need b.L hw hi.1)
            (fun h => absurd (ar_fits w.keyed cost after w.need b.L hw hi.1) (of_decide_eq_false h))
        | false =>
          have hw : after = w.need := hwf
          simp only [stepNow, hJf]
          exact sign_inv b w _ cost after false hi
            (fun h => ar_auto w.keyed ready cost after w.need b.L
              (room_true room hR _ _ _ _ (and_right _ _ h)) hw)
            (fun _ => rfl)
      | write due cost after =>
        have hcalm : w.keyed + cost + after ≤ b.L := hc
        cases hd : waits true false due with
        | true =>
          simp only [stepNow, hd]
          exact hi
        | false =>
          simp only [stepNow, hd]
          exact sign_inv b w _ cost after false hi (fun _ => hcalm) (fun _ => rfl)
      | prove cost after =>
        have hcalm : w.keyed + cost + after ≤ b.L := hc
        exact sign_inv b w _ cost after false hi (fun _ => hcalm) (fun _ => rfl)
      | heal cost =>
        exact sign_inv b w _ cost w.need false hi (fun h => room_true room hR _ _ _ _ h)
          (fun _ => rfl)
      | person cost after =>
        have hcalm : w.keyed + cost + after ≤ b.L := hc
        exact sign_inv b w _ cost after false hi (fun _ => hcalm) (fun _ => rfl)
      | remove batch worst after =>
        have hw : after ≤ worst := hwf
        exact sign_inv b w _ batch after false hi
          (fun h => ar_remove w.keyed batch after worst b.L hw (room_true room hR _ _ _ _ h))
          (fun _ => rfl)
      | prune kept =>
        exact And.intro (ar_prune w.keyed kept w.need b.L hwf hi.1) hi.2

    def Init (w : World) : Prop := w.keyed = 0 ∧ w.need = 0 ∧ w.stuck = false
    def NextNow (b : Budgets) (judges : Bool → Bool) (waits : Bool → Bool → Bool → Bool)
        (room : Nat → Nat → Nat → Nat → Bool) (w w' : World) : Prop :=
      ∃ e, WF w e ∧ Calm b w e ∧ w' = stepNow b judges waits room w e
    def SpecNow (b : Budgets) (judges : Bool → Bool) (waits : Bool → Bool → Bool → Bool)
        (room : Nat → Nat → Nat → Nat → Bool) : StateMachine World :=
      StateMachine.mk Init (NextNow b judges waits room)

    -- Spec |= [](keyed + need <= L and not stuck) for the rule as shipped, over a
    -- stop and a room exact to their specs and any writer's wait (a writer that waits
    -- writes nothing, and one that writes is Calm), for every behaviour whose steps
    -- are faithful and calm, at every size and in either dimension.
    theorem spec_now_never_wedges_when_calm (b : Budgets) (judges : Bool → Bool)
        (hJ : JudgesExact judges) (waits : Bool → Bool → Bool → Bool)
        (room : Nat → Nat → Nat → Nat → Bool) (hR : RoomExact room) :
        Satisfies (SpecNow b judges waits room) (Always (Lift (Inv b))) :=
      invariantSatisfies (SpecNow b judges waits room) (Inv b)
        (fun w h => And.intro (ar_init w.keyed w.need b.L h.1 h.2.1) h.2.2)
        (fun w w' hi hn => Exists.elim hn (fun e he =>
          Eq.mpr (congrArg (Inv b) he.2.2)
            (now_preserves_when_calm b judges hJ waits room hR w e he.1 he.2.1 hi)))

    -- The two readings the code runs: keyed records, and wire bytes, where the
    -- tender's bytes have no stop and a proof admission no line of their own.
    theorem records_never_wedge_when_calm (judges : Bool → Bool) (hJ : JudgesExact judges)
        (waits : Bool → Bool → Bool → Bool) (room : Nat → Nat → Nat → Nat → Bool)
        (hR : RoomExact room) :
        Satisfies (SpecNow records judges waits room) (Always (Lift (Inv records))) :=
      spec_now_never_wedges_when_calm records judges hJ waits room hR
    theorem bytes_never_wedge_when_calm (bound : Nat) (judges : Bool → Bool)
        (hJ : JudgesExact judges) (waits : Bool → Bool → Bool → Bool)
        (room : Nat → Nat → Nat → Nat → Bool) (hR : RoomExact room) :
        Satisfies (SpecNow (bytesOf bound) judges waits room) (Always (Lift (Inv (bytesOf bound)))) :=
      spec_now_never_wedges_when_calm (bytesOf bound) judges hJ waits room hR

    -- ===========================================================================
    -- 5. The proposed rule needs no Calm.
    -- ===========================================================================

    theorem fix_preserves (b : Budgets) (judges : Bool → Bool) (hJ : JudgesExact judges)
        (waits : Bool → Bool → Bool → Bool) (room : Nat → Nat → Nat → Nat → Bool)
        (hR : RoomExact room) (w : World) (e : Event) (hwf : WF w e) (hi : Inv b w) :
        Inv b (stepFix b judges waits room w e) := by
      cases e with
      | write due cost after =>
        cases hd : waits true false due with
        | true =>
          simp only [stepFix, hd]
          exact hi
        | false =>
          simp only [stepFix, hd]
          exact sign_inv b w _ cost after false hi (fun h => room_true room hR _ _ _ _ h)
            (fun _ => rfl)
      | prove cost after =>
        exact sign_inv b w _ cost after false hi
          (fun h => room_true room hR _ _ _ _ (and_right _ _ h)) (fun _ => rfl)
      | person cost after =>
        exact sign_inv b w _ cost after false hi (fun h => room_true room hR _ _ _ _ h)
          (fun _ => rfl)
      | tend rot ready cost after =>
        exact now_preserves_when_calm b judges hJ waits room hR w (Event.tend rot ready cost after)
          hwf True.intro hi
      | heal cost =>
        exact now_preserves_when_calm b judges hJ waits room hR w (Event.heal cost) hwf True.intro hi
      | remove batch worst after =>
        exact now_preserves_when_calm b judges hJ waits room hR w (Event.remove batch worst after)
          hwf True.intro hi
      | prune kept =>
        exact now_preserves_when_calm b judges hJ waits room hR w (Event.prune kept) hwf True.intro hi

    -- ===========================================================================
    -- 6. Counterexamples the kernel evaluates: each rule before PF6 (the mutants
    --    the Rust tests run as controls) wedges where the shipped rule holds, and
    --    the shipped rule wedges without each Calm premise.
    -- ===========================================================================

    def run (step : World → Event → World) (w : World) : List Event → World
      | [] => w
      | e :: es => run step (step w e) es
    def nowIn (b : Budgets) (judges : Bool → Bool) : World → Event → World :=
      stepNow b judges spec_writer_waits roomNat
    def now : World → Event → World := nowIn records spec_stop_judges
    def fix : World → Event → World := stepFix records spec_stop_judges spec_writer_waits roomNat

    -- HEAL_IGNORES_NEED: a heal weighs only the hard bound (F4). At keyed + need = L
    -- it takes a Wrap of the reserve, and the second due re-key is refused.
    def healIgnoresNeed (w : World) : Event → World
      | Event.heal cost => sign w (decide (w.keyed + cost ≤ records.L)) cost w.need false
      | e => now w e
    theorem heal_ignoring_need_takes_the_reserve :
        (run healIgnoresNeed (World.mk 12000 288 false)
          [Event.heal 1, Event.tend true 0 144 144, Event.tend true 0 144 0]).stuck = true := by
      decide
    theorem shipped_heal_keeps_the_reserve :
        (run now (World.mk 12000 288 false)
          [Event.heal 1, Event.tend true 0 144 144, Event.tend true 0 144 0]).stuck = false := by
      decide

    -- FILL_OWN_TARGET_ONLY: a second removal whose worst case counts its own target
    -- alone (142) takes the first one's reserve (F1); counting every owed re-key
    -- (426), as RemovalFill does, it refuses, and the first one's re-keys fit.
    theorem own_target_fill_takes_the_first_reserve :
        (run now (World.mk 11800 0 false)
          [Event.remove 0 284 284, Event.remove 142 142 426, Event.tend true 0 142 284,
           Event.tend true 0 142 142, Event.tend true 0 142 0]).stuck = true := by decide
    theorem shipped_second_removal_refuses :
        (run now (World.mk 11800 0 false)
          [Event.remove 0 284 284, Event.remove 142 426 426, Event.tend true 0 142 142,
           Event.tend true 0 142 0]).stuck = false := by decide

    -- STOP_HOLDS_REMOVALS: the stop judges every re-key. At 8,600 keyed a removal's
    -- 142-record rotation is held, and every writer waits for good.
    theorem stop_holding_removals_wedges :
        (run (nowIn records (fun _ => true)) (World.mk 8600 142 false)
          [Event.tend true 0 142 0]).stuck = true := by decide
    theorem exemption_signs_it :
        (run now (World.mk 8600 142 false) [Event.tend true 0 142 0]).stuck = false := by decide

    -- AUTOMATIC_IGNORES_OWED: the tender weighs the stop alone (F2). A large lab
    -- (about 4,000 seats): the removal reserves 8,000, a coalesce no removal made
    -- fills to the stop, and the rotation no longer fits.
    def tenderIgnoresOwed (w : World) : Event → World
      | Event.tend false ready cost after =>
          sign w (roomNat (w.keyed + ready) cost 0 records.S) cost after false
      | e => now w e
    theorem tender_ignoring_owed_wedges_a_large_lab :
        (run tenderIgnoresOwed (World.mk 4000 0 false)
          [Event.remove 0 8000 8000, Event.tend false 0 4500 8000,
           Event.tend true 0 4000 4000]).stuck = true := by decide
    theorem shipped_tender_holds_it :
        (run now (World.mk 4000 0 false)
          [Event.remove 0 8000 8000, Event.tend false 0 4500 8000,
           Event.tend true 0 4000 4000]).stuck = false := by decide

    -- FILL_IGNORES_BYTES: in the byte reading (a bound of 1,000 bytes), a removal
    -- that weighs records alone signs a batch whose re-keys' 600 bytes no longer fit
    -- (F6); the shipped fill refuses it, signing nothing.
    def bytes : Budgets := bytesOf 1000
    def fillIgnoresBytes (w : World) : Event → World
      | Event.remove batch _ after => sign w (decide (w.keyed + batch ≤ bytes.L)) batch after false
      | e => nowIn bytes spec_stop_judges w e
    theorem fill_ignoring_bytes_wedges :
        (run fillIgnoresBytes (World.mk 600 0 false)
          [Event.remove 10 600 600, Event.tend true 0 300 300, Event.tend true 0 300 0]).stuck
          = true := by decide
    theorem shipped_fill_refuses_the_bytes :
        (run (nowIn bytes spec_stop_judges) (World.mk 600 0 false) [Event.remove 10 600 600]).keyed
          = 600 := by decide

    -- WITHOUT CALM, F3: proof admissions below the 9,216 line take the room the
    -- due re-keys owe; the proposed rule refuses the admission.
    theorem shipped_rule_lets_a_proof_admission_take_the_reserve :
        (run now (World.mk 9000 3000 false)
          [Event.prove 400 3000, Event.tend true 0 1500 1500, Event.tend true 0 1500 0]).stuck
          = true := by decide
    theorem proposed_rule_refuses_the_admission :
        (run fix (World.mk 9000 3000 false)
          [Event.prove 400 3000, Event.tend true 0 1500 1500, Event.tend true 0 1500 0]).stuck
          = false := by decide

    -- WITHOUT CALM, F4: a person's 200 other writes take the room at 12,000.
    theorem shipped_rule_lets_person_writes_take_the_reserve :
        (run now (World.mk 12000 0 false)
          [Event.remove 0 284 284, Event.person 200 284, Event.tend true 0 142 142]).stuck
          = true := by decide
    theorem proposed_rule_refuses_the_person_write :
        (run fix (World.mk 12000 0 false)
          [Event.remove 0 284 284, Event.person 200 284, Event.tend true 0 142 142]).stuck
          = false := by decide

    -- ===========================================================================
    -- 7. The answer sheet: the three kernels on a grid, one bit each. Bits 0 to
    --    1295 are spec_keeps_room over every held, cost, need and limit in
    --    0, 1, 2, 3, 2^63, 2^64 - 1 (limit fastest); 1296 and 1297
    --    spec_stop_judges of false and true; 1298 to 1305 spec_writer_waits over
    --    its three Bools (the last fastest); the rest of the last word is zero.
    --    Sixty-four bits to a word (Clean's numerals fit 64 bits), the lowest bit
    --    index highest. The Rust test `capacity::tests::answer_sheet_matches_the_island`
    --    recomputes it from the Rust kernels and reads the words below from this
    --    file, which holds the Rust keeps_room to spec_keeps_room on the grid until
    --    E6 imports it (Trust's I2').
    -- ===========================================================================

    def gridValue : Nat → UInt64
      | 0 => 0
      | 1 => 1
      | 2 => 2
      | 3 => 3
      | 4 => 9223372036854775808
      | _ => 18446744073709551615

    def gridBool (i : Nat) : Bool := decide (i % 2 = 1)

    def bitWith (room : UInt64 → UInt64 → UInt64 → UInt64 → Bool) (i : Nat) : Bool :=
      if i < 1296 then
        room (gridValue (i / 216)) (gridValue ((i / 36) % 6)) (gridValue ((i / 6) % 6))
          (gridValue (i % 6))
      else if i < 1298 then spec_stop_judges (gridBool (i - 1296))
      else if i < 1306 then
        spec_writer_waits (gridBool ((i - 1298) / 4)) (gridBool ((i - 1298) / 2))
          (gridBool (i - 1298))
      else false

    def wordWith (room : UInt64 → UInt64 → UInt64 → UInt64 → Bool) (w : Nat) : Nat → Nat
      | 0 => 0
      | Nat.succ n => 2 * wordWith room w n + (if bitWith room (64 * w + n) then 1 else 0)

    def sheetWith (room : UInt64 → UInt64 → UInt64 → UInt64 → Bool) (w : Nat) : Nat → List Nat
      | 0 => []
      | Nat.succ n => wordWith room w 64 :: sheetWith room (w + 1) n

    -- Bits 0 to 1343, as 21 words.
    def sheet : List Nat := sheetWith spec_keeps_room 0 21

    theorem sheet_is : sheet =
      [0xfdf3c70c17cf1c30, 0x403c70c30401c30c, 0x30400c1041000040, 0x7cf1c30403,
       0xc70c30401c30c304, 0xc30c3040041041, 0x3c70, 0xc30401c30c30400c, 0x30c30400c30c3040,
       0x410410000000000, 0x1c30c30400c30c, 0x30400c30c30400c3, 0xc30400410410000, 0xc104100,
       0x41041000041041, 0x410410000000, 0x4, 0x0, 0x0, 0x0, 0x810000000000] := by decide

    end AtermMeshCapacity
}
