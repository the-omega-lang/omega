# Spec composition: `+` everywhere, inline multi-bounds, no provisioning

## Task Description

- **What is being asked:** Replace Omega's three overlapping ways of saying
  "these specs together" with one. Concretely:
  1. `spec X = A | B;` becomes `spec X = A + B;`
  2. Generic parameters gain inline multi-bounds: `<T: A + B>`, so a helper
     alias is no longer required to require two specs
  3. Spec **provisioning** (`spec X : A, B`) is **removed from the language**
  4. A spec alias becomes non-conformable, matching its stated meaning

- **Purpose:** Omega currently has three mechanisms for one concept, with three
  different separators and three different satisfaction rules:

  | Form | Separator | Satisfied by |
  |---|---|---|
  | `spec X : A, B { }` | `,` | one flattened `conform T to X` block |
  | `spec X = A \| B;` | `\|` | separate `conform T to A` + `conform T to B` |
  | (no inline form) | — | — |

  That is the "two mechanisms for one concept is a permanent tax" problem, at
  three. After this change there is one separator (`+`), one satisfaction rule
  (conform to each spec separately), and one derivation mechanism (blanket
  conformance).

  **A note on the word.** The `spec X : A` form is called a *dependency*
  throughout the compiler (`ResolvedSpecType.dependencies`,
  `parse_optional_dependencies`), but that name describes Rust's and Swift's
  feature, not Omega's. In those languages `B : A` is a **requirement**:
  implementing `B` obliges you to implement `A` separately. In Omega it is
  **provisioning**: `conform T to X` *supplies* `A`'s methods, in `X`'s block,
  and registers `T` for both.

  Omega has therefore never had requirements — so this plan does not "remove
  dependencies" and does not switch models. It removes provisioning, and the
  misleading field name goes with it. Worth stating plainly because the syntax
  borrowed Rust's spelling without its meaning, which is exactly the kind of
  false friend that traps everyone arriving from Rust and that gets baked in
  once an ecosystem grows around it.

- **Reasoning:**

  The decisive question during review was: *why does a spec need requirements
  at all?* A spec is a contract about what an implementer provides. `Ord` needs
  `compare`. Demanding `Eq` alongside it is not part of implementing `Ord` — it
  is a claim about types that happen to be ordered, and it belongs where that
  claim is made (a bound, or a blanket), not levied on everyone who implements
  the spec.

  Each justification for provisioning was checked against the actual tree and
  did not survive:

  - *"Default bodies can call the requirement's methods."* **Measured: no
    `Ord` default calls an `Eq` method.** All six call only `compare`,
    `greater_than`, `less_than`, `less_or_equal`, `greater_or_equal`. And a
    default that genuinely needs another spec is better expressed as a blanket
    (`conform<T: Ord> T to Sortable { … }`), which puts the constraint on the
    derivation that needs it rather than on every implementer.
  - *"A `T: Ord` bound implies `T: Eq`."* **Measured: nothing in the tree
    bounds on `Ord` at all.** And this only ever mattered because `T: Ord + Eq`
    was not spellable — which item 2 fixes.
  - *"It is a semantic claim."* Provisioning forces the supplied conformance to
    *exist*, never to *agree*. Nothing checks that `compare(a,b).is_eq()`
    matches `a.equals(b)`. It buys a compulsory declaration and a comment.
  - *"Without it you repeat `T: Ord + Eq` at every use site."* Answered by the
    alias: `spec Comparable = Ord + Eq;` names the combination once, imposing
    nothing on implementers.

  **Blanket conformance already subsumes what provisioning does, and does it
  better** — and the two are currently in direct conflict. Verified:

  ```
  spec Ord2 : Eq2 { … }                                  # with provisioning
  conform<T: Ord2> T to Eq2 { equals(…) { … } }          # blanket derivation
  conform S to Ord2 { compare(…) { … } }
  → error: 'S' does not implement spec 'Eq2': missing 'equals'
  ```

  Removing `: Eq2` makes the identical program compile and run. So declaring a
  provisioning *disables* the better mechanism: the flattening check demands
  the methods inline and errors before the blanket is ever consulted.

  **Prior art agrees.** No mainstream language has a "implementing X also
  satisfies Y" declaration form. Rust's `trait B: A` and Swift's
  `protocol B: A` are requirements, and the way both express derivation is a
  blanket/conditional impl. Rust's trait aliases (`trait X = A + B;`,
  [RFC 1733](https://rust-lang.github.io/rfcs/1733-trait-alias.html), unstable
  since 2016) are explicitly *"not a separately implable trait but merely sugar
  for writing the full bound out"* — exactly the alias semantics requested
  here, including non-conformability.

  **Alternatives rejected:**
  - *Convert `spec X : A` to Rust's **requirement** semantics rather than
    deleting it.* This would be adding a feature Omega never had, not
    preserving one. Better than provisioning, but it still imposes on
    implementers, and once `+` and blankets exist it earns nothing measurable.
    Rejected as a mechanism whose entire remaining value is documentation.
  - *Keep provision (today's behaviour) and just add `+`.* Leaves the
    `Eq::equals` link bug, the `Successor` duplication pattern, the
    double-registration ambiguity, and the blanket conflict in place.
  - *Delete the alias too, leaving only inline `+`.* Rejected: the alias is
    what makes removing provisioning free, by naming a recurring combination
    without imposing a conformance.

- **Resolved concerns:**
  1. **`conform T to <alias>` compiles today**, contradicting the alias's
     stated meaning. It is rejected by this plan. Nothing in the tree conforms
     to an alias, so there is no migration.
  2. **Blanket precedence** must be redefined for bound sets — see below. This
     is the one part that can silently miscompile if done casually, and it is
     specified conservatively.
  3. **An all-defaults spec still needs an explicit (possibly empty) conform
     block.** Verified. Conformance stays nominal opt-in; this plan does not
     change that.
  4. **Method identity is `(spec, name, signature)`, and one structure does
     not know that.** `FlattenedSpecFn` (`analysis/specs.rs:6`) carries
     `name`/`fn_type` and no owning spec, and `flatten_spec_into` emits a
     name-keyed list that serves *both* conformance checking and vtable slot
     construction. Every identity bug found during review traces to that one
     omission, and removing provisioning alone does **not** fix it:
     - the `Eq::equals` link failure (mangler writes `Ord::equals`, resolver
       reads `Eq::equals`), and
     - a `spec *` object over a conjunction silently picking the first
       same-named slot.

     Measured: with `spec A { tag; }`, `spec B { tag; }` and `S` conforming to
     both, static dispatch correctly yields 1 and 2 through `T: A`/`T: B`
     bounds, **static dispatch through a conjunction bound is already rejected**
     (`ambiguous reference to overloaded 'tag'`), but `spec *AB` compiles and
     silently returns A's. Same program, two answers by dispatch strategy.

## Technical Details

### The end state

Four orthogonal concepts, no overlap:

```
spec X { ... }                       # a contract: what an implementer provides
spec X = A + B;                      # a name for a conjunction; not conformable
f<T: A + B>(...)                     # an unnamed conjunction, at a bound
conform<T: A> T to B { ... }         # derive one conformance from another
```

`core::cmp` becomes:

```
spec Eq  { equals(…); not_equals(…) { … } }
spec Ord { compare(…); less_than(…) { … } … }        # no `: Eq`
conform<T: Ord> T to Eq { equals(*self, o) => bool { self.compare(o).is_eq() } }
```

The blanket is the *convenience* that provisioning was reaching for, written
once instead of restated in twelve `conform $T to Ord` blocks — and a type
wanting a faster `equals` still writes a concrete `conform T to Eq`, which beats
a blanket under existing precedence.

### What changes

**`compiler/omega-parser`**
- `ast/generics.rs`: `GenericParam.bound: Option<Type>` becomes a list
  (`bounds: Vec<Type>`, empty = unbound). Its doc comment currently says "Only
  one bound is ever parsed here … a function needing several unrelated specs
  names an alias spec instead" — that rationale is deleted.
- `parser/item.rs`: `parse_optional_dependencies` (line 565) is **deleted**;
  `parse_spec_def` no longer accepts `:` after a spec name. The alias arm's
  `while p.eat(&TokenKind::Pipe)` becomes `Plus`.
- Bound parsing accepts `A + B + C`. `+` after a type in bound position is
  unambiguous — types contain no `+`.
- `ast/statement/spec.rs`: `SpecStmt.dependencies` is repurposed to hold the
  alias members only (it already does for aliases), or renamed to say so.

**`compiler/omega-hir`** — mirror the AST changes (`hir.rs`, `lower.rs:479`).

**`compiler/omega-analyzer`**
- `ResolvedSpecType.dependencies` (`resolved_type.rs:337`) is removed or
  narrowed to alias members. Every transitive-dependency walk goes with it.
- `analysis/specs.rs`: the flattening check that requires a conform block to
  supply its provisioned specs' bare requirements is **deleted** — this is the
  change that removes provisioning.
- `check_generic_bound` becomes plural: a bound is satisfied when the concrete
  type conforms to *every* spec in the set.
- A spec alias used as a bound expands to its members before checking.

**`compiler/omega-driver`**
- `items.rs:627` (`.any(|g| g.bound.is_some())`) and `items.rs:744` (`let Some(bound) = param.bound`)
  iterate the list.
- `conformances.rs`: `ConformanceRole::Derived` and the stand-in registration
  (line 724) are **deleted** — with no provisioning, no block registers a
  conformance it was not named for. `transitive_dependency_ids` (line 934) goes
  with it.
- **Blanket precedence** (`compare_conformance_precedence`, line 850) is
  rewritten from "first bound, compared by transitive dependency" to **bound-set
  subset**: a strict superset of bounds is more specific. This *generalizes* the
  rule already there — the existing comment reasons that "the empty bound set is
  a subset of every other," which is exactly this rule at its degenerate case.
  Incomparable sets (`{A,B}` vs `{A,C}`) produce the existing
  `AmbiguousConformance` rather than an arbitrary pick.

**`runtime/core`**
- `cmp.omg`: `Ord : Eq` becomes standalone, plus the derivation blanket.
- `primitives/numerics.omg`, `primitives/char.omg`: `conform $T to Ord` blocks
  drop their inline `equals` (now supplied by the blanket, or by a concrete
  `conform $T to Eq` if a faster one is wanted — decide per type, do not
  duplicate).
- `range.omg`: `spec Steppable = Successor | Ord;` becomes `= Successor + Ord;`,
  or is deleted in favour of inline `conform<T: Successor + Ord>`. Prefer
  deleting it: it exists only because inline bounds did not.
- `range.omg`'s `RangeIterator::next` can return to the readable
  `self.current.equals(self.end)` once the link bug is gone — the
  `compare(…).is_eq()` workaround and its comment come out.

**`examples/dev/dev.omg:470`** — `spec Mammal : Animal, Dummy` needs separate
conformances, or becomes an alias.

**Docs** — `08-specs.md` (provisioning section deleted, `+` documented in all
three positions, alias non-conformability stated), `06-generics.md` (multi-bounds),
`14-known-issues.md` (remove the `Eq::equals` link-bug entry; it is fixed by
construction).

### Spec identity, vtable sectioning, and narrowing casts

Two specs may declare the same name and signature, and those are **different
functions**. Static dispatch already treats them that way; the flattened list
and the vtable do not.

- `FlattenedSpecFn` gains its **owning spec**. Identity becomes `(spec, name)`
  in conformance checking and in slot construction. `flatten_spec_into` stops
  merging by name.
- A `spec *` object over a conjunction gets a **sectioned vtable**: `[A's
  slots][B's slots]`, in a deterministic spec order, rather than one merged
  list. Sections make the next item free.
- **Narrowing casts.** `<spec *A>x` where `x: spec *AB` and `A` is one of
  `AB`'s specs becomes legal, and is the disambiguation mechanism: it is a
  compile-time-known *offset* onto the section, so the fat pointer's data half
  is untouched and the vtable half is adjusted by a constant. Zero runtime cost,
  no lookup, no concrete type needed. Widening (`<spec *AB>` from `spec *A`) is
  **not** offered -- there is no section to invent.
- **A colliding name through a `spec *` object is rejected** as ambiguous,
  matching what static dispatch already does, with the diagnostic naming the
  candidate specs and pointing at the narrowing cast.

This is independent of removing provisioning and fixes both identity bugs at
their shared root. It is why `spec *T` is *in* scope: `spec *Alias` already
compiles today, so conjunction vtables are reachable now, and inline `+` will
make them common.

### What must not change

- **Conformance stays nominal and opt-in.** An all-defaults spec still needs an
  explicit `conform T to X { }`. Structural satisfaction never counts.
- **Blanket orphan rule** — a blanket may still implement only a spec its own
  package declares.
- **Concrete beats blanket.** The existing origin/role precedence is untouched
  except for the bound-set comparison described above.
- **Object safety rules.** Which specs may become `spec *` objects is
  unchanged (`is_object_safe`, driven by `spec T` return requirements).
- **Static dispatch's ambiguity behaviour.** It already rejects a colliding
  name through a conjunction bound. Dynamic dispatch is being brought *to* that
  behaviour, not away from it.

### Risks and open questions

1. **Removing provisioning is the largest single deletion.** `dependencies`
   threads through ten analyzer/driver files. The executing agent should delete
   it in one step and let the compiler enumerate the call sites, rather than
   trying to predict them.
2. **Precedence is where a silent miscompile could hide.** The subset rule must
   be tested directly (see Testing) — this is the same machinery that once
   selected a blanket body over an author's explicit `conform`, a bug that only
   an end-to-end run caught.
3. **`spec *T` vtables may reference provisioned slots.** Verify before deleting
   `ResolvedSpecType.dependencies`; if a vtable builds from the flattened set,
   that consumer needs its own answer rather than a mechanical removal.

## Implementation Plan

1. **Inline multi-bounds, parser-side only.** `GenericParam.bound` → `bounds:
   Vec<Type>`, parse `A + B`, mirror into HIR. Bound *checking* still only
   consults the first entry, so behaviour is unchanged and the tree stays
   green. Add a parser test that `<T: A + B>` produces two bounds.

2. **Bound checking honours every entry.** `check_generic_bound` becomes
   plural; `items.rs:627`/`744` iterate. `<T: A + B>` now genuinely requires
   both. Verifiable against a user type conforming to only one.

3. **`|` → `+` for aliases**, and reject `conform T to <alias>`. Migrate
   `range.omg` and `dev.omg`.

4. **Blanket precedence by bound subset**, with `AmbiguousConformance` for
   incomparable sets. Do this *before* removing provisioning, so the two
   changes can be bisected apart if a conformance selection regresses.

5. **Remove provisioning.** Delete `parse_optional_dependencies`, the flattening
   check, `ConformanceRole::Derived`, the stand-in registration, and
   `transitive_dependency_ids`. Migrate `core::cmp` to standalone `Ord` plus the
   derivation blanket, and drop the now-redundant inline `equals` from the
   numeric and `char` conform blocks.

6. **Give `FlattenedSpecFn` its owning spec.** Identity becomes `(spec, name)`
   through conformance checking and slot construction; `flatten_spec_into`
   stops merging by name. This is the step that fixes the `Eq::equals` link
   failure, so verify it with the minimal repro
   (`same<T: Ord>(a,b) => a.equals(b)`) rather than by inspection.

7. **Section conjunction vtables per spec**, and reject an ambiguous method
   call through a `spec *` object with a diagnostic naming the candidates.

8. **Add narrowing casts** (`<spec *A>x` from `spec *AB`) as the
   disambiguation, implemented as a constant vtable-pointer offset. Reject
   widening.

9. **Collect the winnings.** Delete `Steppable`; `Range`'s conformances bound on
   `T: Successor + Ord` directly. Restore `RangeIterator::next` to `equals`.
   Remove the link-bug entry from `docs/14`.

10. **Docs**, per the list above, plus `08-specs.md` on spec identity, sectioned
    vtables and narrowing casts.

## Testing

- **New cases:** `<T: A + B>` requires both (a type conforming to only one is
  rejected, naming the missing spec); three-way `<T: A + B + C>`; an alias
  bound and an inline bound behave identically; a blanket bounded on a
  conjunction; `spec X = A + B` satisfied by separate conform blocks; `char`
  and integer ranges still iterate (`just test-range`, `just test-char`).

- **Precedence cases, tested by *execution* not just compilation** — each must
  assert which body actually ran:
  - `conform<T: A + B> T to X` beats `conform<T: A> T to X` for a type with both
  - a concrete `conform S to X` beats both
  - `conform<T: A + B>` vs `conform<T: A + C>` for a type with all three →
    `AmbiguousConformance`, not an arbitrary pick
  - unbounded `conform<T> T to X` still loses to any bounded blanket

- **Spec identity cases, all asserted by execution:** two specs declaring the
  same name+signature dispatch to their own bodies through `T: A` and `T: B`
  (already true -- pin it); `same<T: Ord>(a,b) => a.equals(b)` **links and
  runs** (the current link failure); `spec *AB` with a colliding name is
  rejected rather than silently picking; `<spec *A>x` then `x.f()` selects A's
  body and `<spec *B>x` selects B's, proving the sections are distinct and the
  offset is right.

- **Negative cases:** `spec X : A` is a parse error naming the replacement
  (`spec X = A + B;` for a name, or `<T: A + B>` at the bound);
  `conform T to <alias>` rejected, saying an alias names a combination and is
  not itself implementable; `<T: A + B>` where the type conforms to only `A`
  names `B` specifically, not both.

- **Regression risk:** highest in `compiler/omega-driver/tests/conform.rs` (47
  tests, the blanket-precedence suite) and in `core::cmp`'s twelve numeric
  conformances. `just test-multi-print` exercises blanket instantiation across
  package boundaries and is the most likely gate to catch a linkage regression.

- **Gates:** `cargo test`, then `just test-io test-stdio-contract test-core-only
  test-root-layout test-allocator-only test-multi-print test-range test-char
  run-exec`. Symbol tables will change (an `Eq` blanket instantiation replaces
  twelve inline `equals` bodies); record the new baseline rather than treating
  the diff as failure.
