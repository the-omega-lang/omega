# Completing `char`

## Task Description

- **What is being asked:** Finish `char` as a first-class primitive — helper
  methods, an enforceable validity invariant, correct arithmetic rules, and
  range iteration (`'A'..='Z'`). Five items were named; the sections below map
  each to what it actually costs.

- **Purpose:** `char` is currently the least-served type in the language. It
  has no methods, no conformances except `Display`, no supported way to build
  one from a computed value, cannot be iterated, and permits an operation
  (`char + char`) that has no meaning. This plan supplies the missing
  constructor, the methods that were only ever waiting on it, and the operator
  rules — and, as it turns out, needs no new compiler machinery to do any of
  it.

- **Reasoning:**

  `char` **stays a `primitive`**, per the requester: "It won't be a struct, all
  of them will be `primitive`." That settles the largest open question and
  rules out the alternative of making `char` a core library struct (which would
  have needed no privileged conversion at all, at the cost of reworking match
  range patterns and auditing every `primitive`-keyed code path).

  **No new compiler mechanism is needed to construct a `char`.** An earlier
  draft of this plan added a `core`-only `u32 -> char` cast privilege, on the
  assumption that `allows_cast_into` made `char` unconstructible outside the
  compiler. It does not. A pointer round-trip already does it, from any
  package, with no privilege:

  ```
  mut n : u32 = 0xD800u32;
  c := *<*char>&n;          # a surrogate, in a char, today
  ```

  Verified: this compiles and runs from an ordinary user package. So
  `char::from_u32` is writable in plain Omega right now, and the entire
  privileged-conversion design is unnecessary.

  **What this means for the invariant, stated honestly:** `char`'s validity is
  a *convention upheld by the supported path*, not a property the language
  enforces. It cannot be enforced while pointer casts are unrestricted, and
  restricting those would fight Omega's "pointers are honest addresses" model
  for no realistic gain — a systems language competing with C has to leave that
  door open. Rust is in the same position; the difference is that Rust's bypass
  requires the word `unsafe`, and Omega has no such marker.

  The direct `<char>some_u32` ban is therefore kept, but reclassified: it is an
  **ergonomic guardrail** that routes people to the checked constructor, not a
  soundness barrier. That is worth keeping — the direct spelling is what
  someone reaches for first — but it must be documented as what it is, because
  a restriction that implies a guarantee it does not provide is worse than
  either extreme.

  **Alternatives rejected:**
  - *A `core`-only cast privilege, or a compiler intrinsic* — both add
    machinery to protect an invariant that is already bypassable. Ceremony
    without a guarantee.
  - *Restricting pointer casts so validity-bearing types cannot be
    reinterpreted* — would close the hole, but touches every `<*T>` cast and
    contradicts the pointer model. Wrong trade for this language.
  - *Dropping the invariant and making `char` a transparent `u32`* — the
    convention still has real value: it makes the supported path total and
    keeps `Display`/UTF-8 encoding meaningful.
  - *Keeping `char` arithmetic* — see below; it is sound but meaningless, and
    banning it costs nothing.

- **Resolved concerns:**
  1. **Item 1 (`u8` castable to `char`) is already true.** `allows_cast_into`
     permits `Char | U8` as sources. Verified compiling. This item needs a
     regression test, not an implementation.
  2. **Item 2 (helper methods) needs no new mechanism.** Verified: a
     `primitive char { is_ascii, is_digit, len_utf8 }` block compiles today,
     because casting *out* of `char` is already legal. Pure library work.
  3. **`char` arithmetic is banned entirely** (requester's decision), matching
     Rust, which has no `Add for char` at all. Zero migration cost: nothing in
     the tree does `char` arithmetic — `String::push` already begins
     `cp := <u32>c;`.
  4. **`char + char` is sound today, not a hole.** Both operands coerce to
     `u32` before the operator applies, so an invalid `char` can never be
     produced — no `char` is produced at all. This change removes a
     meaningless operation; it does not close a soundness bug.
  5. **`str` validity is designed but not built** (requester's decision). The
     final section specifies it concretely enough to execute later without
     re-deciding anything.
  6. **A documented soundness claim in the tree is false and must be
     corrected.** `docs/01-primitives.md` and `ResolvedType::integer_domain`'s
     comment (`resolved_type.rs:955`) both justify not carving the surrogate
     hole out of `char`'s match-exhaustiveness domain on the grounds that "a
     real `char` value can never actually land in that hole in the first place
     -- char literals are validated through `char::from_u32` at parse time."
     The pointer round-trip above produces exactly such a value without a
     literal. The *conclusion* (treat the domain as contiguous) is still fine
     -- it makes exhaustiveness stricter, never more permissive -- but the
     stated reason is wrong and has to say so.

## Technical Details

### No new compiler mechanism

Everything below is either an analyzer *restriction* or ordinary Omega.
`allows_cast_into` keeps its current rule unchanged; only its diagnostic gains
a help line pointing at `char::from_u32`.

### What changes

**`compiler/omega-analyzer/src/analysis/exprs.rs`**

1. `allows_cast_into` — **unchanged behavior**; add a help line naming
   `char::from_u32` so the rejection points somewhere useful.

2. **Ban `char` arithmetic** by removing `Self::Char => Some(ResolvedType::U32)`
   from `ResolvedType::arithmetic_repr` (`resolved_type.rs:861`). This one
   deletion does the whole job, and is why it is the chosen approach:
   - `coerce_for_binary_op` no longer coerces `char`, so the validity loop in
     `analyze_binary_op` sees `Char` and admits it only for comparisons —
     which is precisely what that loop's existing comment already claims
     happens.
   - `coerce_for_unary_op` no longer coerces `char`, so `~c` fails
     `analyze_bit_not`'s `numeric_kind` test.
   - Comparison is unaffected: `coerce_for_binary_op` returns early for
     `is_comparison() && Char` *before* consulting `arithmetic_repr`, and
     codegen's `MirExpr::BinaryOp` already special-cases `Char` as its own
     4-byte unsigned scalar.

   **The executing agent must verify `arithmetic_repr` has no other consumer**
   that depends on the `Char` case before deleting it. If one exists, gate
   there instead rather than reintroducing coercion.

3. **Pointer arithmetic gets C's rule.** Unlike `char`, pointers keep
   arithmetic — Omega's byte-wise, unscaled model (`p + 1` is a `usize`, cast
   back by hand) is a deliberate no-hidden-behavior choice and stays. Only the
   meaningless combinations go. Permitted:
   - `ptr` OP `ptr` for comparisons and `-` (a distance; C's `ptrdiff_t`)
   - `ptr` `+`/`-` integer, and integer `+` `ptr`

   Everything else — `ptr + ptr`, `ptr * ptr`, `ptr & ptr`, shifts on a pointer
   pair — is rejected. Note Omega currently permits *all* of these, making it
   **more permissive than C** on the one case C singled out.

   This check needs both operands' **pre-coercion** types, which per-operand
   `coerce_for_binary_op` cannot see. Add the check in `analyze_binary_op`
   before the coercion calls at lines 1238–1239.

**`compiler/omega-analyzer/src/error/kind.rs` + `error/render.rs`**

Two new diagnostics. Reusing `InvalidBinaryOperand` would produce "cannot apply
'+' to a value of type 'char'", which states the rule but not the remedy:
- `CharArithmeticNotAllowed { op }` — help: cast first, `<u32>c + 1`.
- `PointerPairArithmetic { op }` — note: only `-` and comparisons are defined
  between two pointers; help: cast to `usize` if raw address arithmetic is
  intended.

**`runtime/core/primitives/char.omg`** — the block stops being empty:

```
primitive char {
    exposed from_u32(value: u32) => Option<char>   # the checked constructor
    exposed is_ascii(*self) => bool
    exposed is_digit(*self) => bool                # ASCII '0'..='9'
    exposed is_alphabetic(*self) => bool           # ASCII only; say so
    exposed is_whitespace(*self) => bool           # ASCII only; say so
    exposed len_utf8(*self) => usize               # 1..=4
    exposed to_ascii_uppercase(*self) => char
    exposed to_ascii_lowercase(*self) => char
}
```

`from_u32` rejects `value > 0x10FFFF` and the surrogate block
`0xD800..=0xDFFF`, then produces the `char` via the pointer round-trip
(`*<*char>&validated`) — ordinary Omega, no privilege. Its doc comment must
say plainly that it is the *supported* way in rather than the only one, and
that the same round-trip can bypass it. Every classifier is written
against `<u32>*self`, needing nothing new. **Keep the ASCII-only ones honestly
named or documented** — a `is_alphabetic` that silently means ASCII is a lie
that gets found at the worst time; full Unicode tables are out of scope and
should be stated as such.

**`runtime/core/primitives/char.omg` (conformances) or `core/cmp.omg`**

- `conform char to Ord` — `char` currently conforms only to `Display`, so it
  supports `<` as an operator but cannot satisfy a `T: Ord` bound. Unblocked
  today; fix it here.
- `conform char to Successor` — `greater_than`/`equals` from comparison,
  `successor` returning `None` at `char::MAX` and **skipping the surrogate
  block**: the successor of `'\u{D7FF}'` is `'\u{E000}'`, not `0xD800`. This is
  the single reason `char` needs a real `Successor` rather than `+1`, and Rust
  hit the identical wall (`Step for char` adds `0x800` at the boundary).
- `conform char to Bounded` — `min()` is `<char>0u8` (expressible today);
  `max()` needs `from_u32(0x10FFFF)` and the `core` cast.

Together these deliver item 5: `for c in 'a'..='z'` starts working through the
ordinary `ToIterator`/`Iterator` path, with no range machinery touched.

**Docs:** `docs/01-primitives.md` (arithmetic section — `char` leaves the
"coerces to its `arithmetic_repr`" group entirely; the surrogate-hole note's
"nothing synthesizes a `char` by counting" caveat becomes false once
`Successor` exists and must be rewritten to say the skip is what preserves it),
`docs/13-core-library.md` (char's method set), `docs/14-known-issues.md` (drop
the `char`-not-iterable entry; add the ASCII-only classifier limitation).

### What must not change

- **`char`'s representation, and `char` stays a `primitive`.** Not a struct.
- **Match range patterns (`'A'..='Z'`) already work** and are untouched. Item 5
  is about *iteration*, which is a different mechanism.
- **`<char>u8` already works** — regression test only, no implementation.
- **Pointer arithmetic stays byte-wise and unscaled.** Do not adopt Rust's
  element-scaled `.add()`; that is the hidden scaling Omega deliberately
  rejects.
- **`bool` is already correct** — it has no `arithmetic_repr`, so `true + true`,
  `<`, shifts and `~` are all already rejected, and `& | ^` are its logical
  operators. Nothing to do.
- **`str` validity** — designed below, not built.

### Risks and open questions

1. **`arithmetic_repr`'s other consumers.** Deleting the `Char` case is the
   whole arithmetic ban; verify nothing else depends on it first. Flag rather
   than work around.
2. **Surrogate skip correctness.** `successor` must be tested exactly at both
   boundaries (`0xD7FF -> 0xE000`) and at `char::MAX -> None`. An off-by-one
   here produces invalid `char`s, which is the entire thing this plan exists
   to prevent.
3. **The invariant is conventional, not enforced.** Nothing in this plan
   changes that, and nothing can without restricting pointer casts. Do not add
   machinery that implies otherwise, and do not write a comment claiming a
   `char` is always valid — write that the supported path always produces a
   valid one.

## Implementation Plan

1. **Regression-test what already works**: `<char>u8` compiles, `<char>u32`
   does not, `char` comparison works, `'A'..='Z'` match patterns work. These
   pin behavior the rest of the plan must not break.

2. **Ban `char` arithmetic**: delete the `Char` case from `arithmetic_repr`,
   after verifying it has no other consumer. Add `CharArithmeticNotAllowed`
   with its help line. Tree must build; `String::push` must still compile
   untouched.

3. **Pointer pair rule**: add the pre-coercion check in `analyze_binary_op`,
   plus `PointerPairArithmetic`. `ptr - ptr` and `ptr ± int` must still work —
   `core::primitives::slices` and `std::list` exercise pointer arithmetic.

4. **`char::from_u32` + classifiers** in `primitive char`. Now `core` can
   construct a `char`.

5. **`conform char to Ord`**, then `Successor` and `Bounded`. Range iteration
   should start working with no changes to `core::range`.

6. **Docs**, per the list above — including the corrected surrogate-hole
   rationale in both `docs/01-primitives.md` and `resolved_type.rs`.

## Testing

- **New cases:** `from_u32` accepts `0x41`/`0x10FFFF`, rejects `0x110000` and
  every boundary of `0xD800..=0xDFFF`; each classifier at its boundaries
  (`'0'`/`'9'`, `'a'`/`'z'`, `0x7F`/`0x80`); `len_utf8` at 1/2/3/4-byte
  boundaries; `char` satisfies a `T: Ord` bound; `for c in 'a'..='z'` yields 26;
  a range spanning the surrogate hole yields the right count and never produces
  a surrogate; `<char>u8` still compiles.

- **Negative cases:** `'a' + 'b'`, `'a' + 1`, `'a' - 'a'`, `~'a'`, `'a' & 'b'`
  → `CharArithmeticNotAllowed`, help naming `<u32>c`. `ptr + ptr`, `ptr * ptr`,
  `ptr & ptr` → `PointerPairArithmetic`. `<char>some_u32` outside `core` →
  today's error plus a help line naming `char::from_u32`. `'a' < 'b'`,
  `ptr - ptr`, `ptr + 1` must all still **succeed** — over-tightening is the
  likely failure mode here.

- **Regression risk:** highest in `analyze_binary_op` (every operator flows
  through it) and in codegen's `Char` comparison special case. `runtime/std/
  string.omg`'s `push` is the canonical `char`-to-`u32` consumer.

- **Gates:** `cargo test`, then `just test-io test-stdio-contract test-core-only
  test-root-layout test-allocator-only test-multi-print test-range run-exec`.
  Symbol tables grow (core gains `char` methods and three conformances);
  record the new baseline rather than treating the diff as failure.

---

## Designed, not built: `str` validity

Specified now so it can be executed later without re-deciding anything. **Do
not implement as part of this plan.**

**The hole.** `byte_pointer_cast_kind` (`analysis/exprs.rs:1628`) treats any
fat-to-fat byte-run cast as a `Reinterpret`, so `<*str>some_u8_slice` compiles
and produces a `*str` over arbitrary bytes. Verified: a slice holding
`0xFF 0xFE 0x41` casts to `*str` today. `String`'s UTF-8 guarantee is therefore
unenforceable — and `String::push` is UTF-8-correct only because *it* is
careful, not because the type prevents otherwise.

**The fix, mirroring `char` exactly — including its honesty about scope:**
- `str::from_utf8(bytes: *[]u8) => Option<*str>` in `primitive str`, validating
  the UTF-8 encoding (well-formed lead/continuation bytes, no overlong forms,
  no surrogates, max `0x10FFFF`) in ordinary Omega. This is the supported way
  in, and the whole of the deliverable.
- Optionally reclassify the `Slice{U8|I8} -> Str` direction of
  `byte_pointer_cast_kind` as a guardrail pointing at `from_utf8`, exactly as
  `<char>u32` does. **Do not treat this as enforcement**: the same pointer
  round-trip that defeats the `char` ban defeats this one, so restricting the
  cast changes which spelling is convenient, not what is possible. Decide
  whether the guardrail earns its disruption; the reverse direction
  (`Str -> Slice`, and both to thin pointers) must stay open regardless, since
  every `str` is a valid byte run.

**Prerequisite to check first:** `runtime/std/string.omg`'s `as_str` and
`core::primitives::strings` both rely on the fat-to-fat cast. Establish whether
they can be expressed through `from_utf8`, or whether `std` also needs a
privileged path, *before* restricting the cast — this is the step most likely
to reveal that the restriction is wider than it looks.
