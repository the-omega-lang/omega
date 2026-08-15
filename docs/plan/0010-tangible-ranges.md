# First-class `Range<T>` in `core`

> **Status: implemented.** This document was revised after execution to match
> what was actually built. Where the design moved during implementation, the
> original intent and the reason it changed are both recorded, since the reason
> is the part that is expensive to reconstruct later.

## Task Description

- **What was asked:** Make ranges tangible values. `my_range := 1..<10;` must
  produce a real `core::range::Range<i32>` that can be stored, passed and
  returned, and `for i in my_range { }` must iterate it through the *ordinary*
  `ToIterator`/`Iterator` protocol. The compiler's bespoke range-driven `for`
  desugaring is deleted, not kept alongside.

- **Purpose:** Removes a whole special-cased machine from the analyzer. `for i
  in a..<b` used to be intercepted before iterator resolution and hand-desugared
  into a three-clause `for` by `analyze_for_in_range` — ~200 lines
  reimplementing loop construction, element-type checking and overflow-safe
  termination that the iterator protocol already does generically. Ranges become
  an ordinary library type and `for ... in` has exactly one code path. This
  serves *abstractions that compile away* and *no hidden behavior*: the loop a
  range produces is written in Omega, in `core`, and the user can read it.

- **Reasoning:**

  `Range<T>` is **inert data**; `RangeIterator<T>` is the **cursor**. Two types.
  This is deliberately *not* what Rust did. Rust's `Range` implements `Iterator`
  directly, which forced `RangeInclusive` to carry a `pub(crate) exhausted: bool`
  in the *value* type — its own source comments admit this exists "to support
  `PartialEq` and `Hash` without a `PartialOrd` bound", i.e. iteration state
  contaminates equality: two identical inclusive ranges compare unequal once one
  has been iterated. It also made `Range` non-`Copy` permanently. Rust is
  unwinding this via RFC 3550 (new `Copy` + `IntoIterator` range types,
  stabilizing since 1.95). Omega has no compatibility burden and starts where
  Rust is trying to arrive.

  This matters more here than in Rust: Omega has no move semantics and no borrow
  checker, so a range that *was* its own cursor would be silently bit-copied on
  every pass and lose iteration progress with nothing to catch it. Value/cursor
  separation is what makes the feature sound at all — and it is directly tested
  (`the_same_range_value_can_be_iterated_twice`).

  **Alternatives rejected:**
  - *`Range` conforms to `Iterator` directly* — the Rust design; unsound under
    Omega's copy semantics.
  - *Separate `Range` / `RangeInclusive` types* to avoid the `inclusive` flag —
    costs a second type plus a spec to abstract over both (Rust needed
    `RangeBounds`). The flag folds to a constant whenever a range is constructed
    at its use site, which is nearly always. Revisit only if the stored case
    measures hot.
  - *Keeping the special-cased `for` desugaring alongside the value type* — two
    mechanisms for one concept.
  - *A `@lang("range")` annotation linking compiler to core type* — rejected by
    the requester: "the whole point of core is to connect together the compiler,
    the platform and the language syntax." A hardcoded path is used instead,
    consistent with the existing hardcoded `core::option::Option` variant order.

## The syntax rule

`..` is the **contextual range operator**. It is the spelling for *"nothing is
written on this side — work it out from where I am"*, and that is its entire
job. `..<` and `..=` are different tokens, and both always carry a written end.

Which side gets inferred is orthogonal to which token is used:

| Written | Start | End |
|---|---|---|
| `a..<b` / `a..=b` | written | written |
| `..<b` / `..=b` | **inferred** | written |
| `a..` | written | **inferred** |
| `..` | **inferred** | **inferred** |

So a leading-open range is perfectly ordinary — `..<b` and `..=b` are valid in
every position, and `..` alone is the match catch-all.

**An end bound may never follow `..`** (`ParseErrorKind::OpenRangeHasEnd`), in
expression, slice index or match pattern alike. `a..b` and `..5` are rejected
for the same single reason: an end turned up after the token that means "no
bound here". Writing an end at all — with or without a start — requires `..<`
or `..=`, because a range that names its end has to say whether the end is in
it.

What an inferred side resolves to is decided by position, and this is the only
positional rule in the feature:

| Position | An inferred side means | Range is a… |
|---|---|---|
| expression | the element type's domain limit, via `Bounded` | value |
| index (`&items[5..]`) | the container's own length | syntax |
| match pattern | the arm's unmatched remainder | syntax |

Contextual inference always beats domain inference. `&items[5..]` is "to the end
of `items`", never `5..=usize::MAX` — which is also why an unsized `*[?]T` base
still errors: no length, nothing to infer from. Consequence, documented rather
than left to be discovered: a *stored* range cannot express "the rest of a
container", since "the rest" is a property of the container.

Bare `..` infers both sides, so standalone it has no type source and is rejected
(`a := ..;`). An expected `Range<T>` is context, so `r : Range<i32> = ..;`
resolves.

## Technical Details

### What changed

**`runtime/core/cmp.omg`** (moved from `runtime/std/cmp.omg`) — `Ordering`,
`Eq`, `Ord`. `core` cannot depend on `std` and `Range` needs an order.

**`runtime/std/primitives.omg`** — the twelve `conform $T to Ord` blocks moved
to `core/numerics.omg`; `Display`/`Hash`/`Default` stayed. Header rewritten: the
"conformance is the explicit package boundary" premise still holds (core owns
`Ord`, so core conforms to it), but the wording had to say so.

**`runtime/core/strings.omg`** — gained `conform str to Eq`, *forced* by the
move: `str` has no declaring package, so under the orphan rule only the package
owning the spec may conform it. Header rewritten; it had claimed these were
inherent methods "without attaching any comparison conformance here", directly
above a comparison conformance.

**`runtime/core/range.omg` (new)** — `Range<T>`, `RangeIterator<T>`,
`spec Successor`, `spec Bounded`, and the two conformances.

**`runtime/core/numerics.omg`** — `Ord`, `Successor` and `Bounded` conformances
for the ten integer types, all macro-generated. `$MAX` is a new macro parameter.
Floats get `Eq` only.

**`compiler/omega-parser/src/parser/expression.rs`** — ranges parse in general
expression position at lowest precedence. `parse_range_tail`'s single
`terminator: &TokenKind` parameter became a structural
`expression_starts_here` test, because ordinary expression position has no
single terminator to name (`r := 1..;`, `f(1..)` and `for i in 1.. { }` all end
the range differently).

**`compiler/omega-analyzer/src/analysis/exprs.rs`** — `HirExpr::Range` builds the
`Range<T>` value: element type from whichever bound is written, else the
expected type; absent bounds become `Bounded::min`/`max` calls.

**`compiler/omega-analyzer/src/analysis/stmts.rs`** — `analyze_for_in`'s
interception deleted, and `analyze_for_in_range` deleted with it (~290 lines
including its now-orphaned `binop` and `number_literal` helpers).

**Errors** — `ForLoopRangeMissingStart`, `ForLoopRangeElementNotSupported` and
`ForLoopRangeBoundTypeMismatch` deleted; `RangeNotAllowedHere` repurposed as the
bare-`..` diagnostic (it was previously unreachable); `RangeNeedsBounded` added;
`MissingSliceEnd` reworded from "a slice over an unsized array must have an
explicit end bound" to "there is no length here to infer a range end from", now
that the rule is general rather than a special case.

**Docs** — `01-primitives.md`, `11-strings-casting-and-slices.md`,
`13-core-library.md`, `14-known-issues.md`, `18-for-in-loops.md`,
`23-standard-library.md`, plus `ast/range.rs`'s own doc comments.

### What deliberately did not change

- **`analyze_slice` still consumes `HirRange` structurally.** Index ranges are
  not values. Positional rule, not an inconsistency.
- **Match range patterns untouched.** Patterns are not expressions.
- **`integer_domain()` kept** — `match` exhaustiveness still needs it.
- **`char` gains nothing.** Out of scope by decision (see below).
- **No `RangeBounds`-style unifying spec, no `step_by`, no reverse iteration,
  no `contains`.**

### Design decisions taken during implementation

1. **`char` is out of scope.** `allows_cast_into` permits only `Char | U8` as
   sources for a cast *into* `char`, so `successor(char)` — which needs
   `char -> u32 -> arithmetic -> char` with a surrogate skip — has no writable
   body, and the compiler has no intrinsic mechanism. The requester ruled this
   needs a deeper structural decision than ranges. `char` gets no `Successor`,
   is not range-iterable, and `docs/01` now records that its surrogate-hole
   soundness argument *depends* on that: stepping a `char` would manufacture the
   very value the argument assumes cannot exist, and `String`'s UTF-8 encoder
   would then emit invalid UTF-8 from an ordinary-looking loop. When
   `char::from_u32` exists, `conform char to Successor` is purely additive.

2. **`Successor` is self-contained, not `Successor : Ord`.** The plan originally
   specified inheritance. Two things were learned:
   - Parent-spec method access through a generic bound **does** work (this was
     the plan's open Risk 1 — verified, and the answer is yes).
   - But Omega *flattens* an inherited spec's methods into the derived spec's
     own conform block — which is why `conform T to Ord` supplies `Eq`'s
     `equals` inline instead of needing a separate `conform T to Eq`.
     Inheriting would therefore force every range element type to restate its
     entire ordering inside its `Successor` block, duplicating the
     `conform T to Ord` that is the canonical place to declare it.

   So `Successor` declares `greater_than`, `equals` and `successor` directly.
   The alternative was rejected on cost, not on feasibility, and `range.omg`
   records that.

3. **`Bounded` is separate from `Successor`.** A type can be steppable without
   having a representable first or last value, and should still work in a
   fully-written range. Only inferred bounds consult `Bounded`.

4. **`usize`/`isize` go through the same macro as every other width.**
   `numerics.omg` refuses width-dependent *literals*, so `$MAX` is derived:
   `~<usize>0` is all bits set at the target's real width, and shifting right by
   one (logically, since it is unsigned) clears the sign bit for the signed
   maximum. Hand-expanding either type instead would silently drop its
   `primitive` block — which is exactly what happened once and is now guarded by
   a test.

5. **Floats are constructible but not iterable.** `f32`/`f64` conform to `Eq`
   but have no total order (`NaN`), so no `Successor`. `Range<f32>` builds fine
   and fails only when iterated.

### Accepted cost

**Range loops generate worse code until MIR optimization exists.** A range loop
is now a `next()` call returning `Option<T>` plus a match, per iteration.
Recovering the old three-clause shape needs two MIR passes that do not exist:
inlining, and scalar replacement of aggregates to dissolve the cursor struct
into registers. **Cranelift will not do this** — its optimizer is far weaker
than LLVM's here, and LLVM is what makes equivalent Rust collapse. This is a
deliberate trade of generated-code quality for uniformity and a much smaller
compiler, recorded in `docs/14-known-issues.md` as the strongest motivating case
for starting the MIR optimizer.

### Defects found and fixed during review

Recorded because each one says something about where this feature is fragile:

1. **`for i in 1.. { }` stopped parsing** (regression). `expression_starts_here`
   counted `{` unconditionally, so the loop body was consumed as the range's end
   bound. Generalizing `terminator` into a context-free predicate discarded
   information the caller had. Fixed by gating `{` on
   `Parser::struct_literals_allowed()` — the same ambient signal that already
   distinguishes a `while`/`for`/`if` header from the block after it.
2. **`for i in a..<b { }` did not parse** (pre-existing, not a regression).
   `parse_range_tail` forced `allow_struct_literals`, overriding the restriction
   the `for` header had set, so `b { ... }` was read as a struct literal. Fixed
   by inheriting the ambient restriction; slice and pattern positions re-allow
   explicitly, since their brackets remove the ambiguity. No code in the tree
   used a variable-bounded range loop, which is why this had survived.
3. **`isize` lost all eight inherent methods.** Its conformances were
   hand-expanded rather than macro-invoked, and the macro also emits the
   `primitive` block. Caught by symbol-table diff (8 removals) and A/B compile.
4. **`f32`/`f64` silently lost `Eq`.** The float macro's conformance was not
   carried over in the `cmp` move.
5. **User types never actually worked.** `RangeIterator::next` used the built-in
   `==`, which is numeric-only — so despite the spec bound, ranges were
   primitive-only in practice. This was masked because the parser bug in (2)
   meant the user-type test never reached analysis. Fixed by `Successor::equals`.
6. **`a..b` was accepted as an exclusive range in expression position** — while
   still being rejected as `OpenRangeHasEnd` in slices and patterns. The same
   syntax meant two different things depending on where it was written, and the
   expression-position meaning was documented nowhere. It came from reading the
   `my_range := 1..10` in the original request as a syntax requirement rather
   than as shorthand.

   **Fixed by making `a..b` flat-out invalid in every position**, per the rule
   at the top of this document: an end bound may never follow `..`, since `..`
   is precisely the spelling for "nothing written here". This leaves leading-open
   ranges untouched — `..<b`, `..=b` and bare `..` are all still valid wherever
   they were — because those are different tokens, not `..` with an end stapled
   on. There is no longer any code path that can construct a bounded range from
   `..`: the `allow_dotdot_end` parameter that permitted it was deleted from
   `parse_range_tail` rather than defaulted off, and three tests pin the
   rejection in expression, slice and pattern position respectively.

## Testing

**`compiler/omega-driver/tests/range.rs` — 22 tests**, compiled against the real
`runtime/core` rather than a stub, since the feature is a claim about what
`core` ships. Covers: binding and iterating a range; field reads; iterating the
same value twice (the value/cursor property); passing through a function
boundary; both `{`-ambiguity shapes; `a..b` rejected in all three positions;
domain inference for `1..` and `..<10`; contextual inference for `&arr[2..]`;
`MissingSliceEnd` on an unsized base; bare `..` rejected standalone and accepted
against an expected type; a user type conforming to `Successor` + `Bounded`
iterating; `RangeNeedsBounded` naming the missing spec; `char` and float
rejection; and regression guards for `isize`'s inherent methods, `usize` ranges,
and inclusive-to-domain-maximum termination.

**`examples/range_demo/` behind `just test-range`** — ten runtime assertions with
a distinct exit code each, linking against `core` alone (which also proves range
iteration needs no allocator and no platform glue). Semantics that only
execution can check live here: exclusive sums to 45, inclusive to 55,
`253u8..=255u8` yields exactly 3 without overflow, an inverted range yields 0,
an open end breaks out correctly, and iterating one range twice counts the same
both times.

**Verified final state:** `cargo test` 151 passed / 0 failed (129 before this
work); `just test-io test-stdio-contract test-core-only test-root-layout
test-allocator-only test-multi-print test-range` all PASS; `run-exec` exit 69;
build warning-clean. Symbol tables: `core` 80 → 226 defined symbols with **zero
removals**, `std` 191 → 91 with every removal explained by the `cmp` move.
