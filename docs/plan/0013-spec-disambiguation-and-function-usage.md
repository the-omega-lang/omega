# Fully-qualified spec function calls: `<Type : Spec>::function(...)`

## Task Description

- **What is being asked:** Complete the three ways to name a spec function.

  1. **A fully-qualified form that always resolves**, naming both halves of the
     function's identity:

     ```
     <i32 : Bounded>::min()              # static
     <Dog : Animal>::make_sound(&dog)    # instance
     ```

  2. **Make `Spec::static_fn()` work**, taking `Self` from the expected type:

     ```
     x : char = Bounded::min();          # Self = char, from the annotation
     ```

     This is Rust's `let x: Foo = Default::default()`, and it is the form
     `std::default::Default` exists to be used with.

  3. **Better ambiguity diagnostics** — name the candidate specs and print the
     qualified spelling for each.

- **Purpose:** Omega currently *detects* conformance ambiguity it cannot let
  the author *resolve*. Measured:

  ```
  conform S to P { make() => Self { ... } }
  conform S to Q { make() => Self { ... } }

  S::make()    → error: conforming static function 'S::make' is ambiguous
  P::make()    → error: this function takes 1 argument but 0 were supplied
  ```

  The first error is correct. The second is the only other spelling, and it
  fails because the qualified form assumes a receiver. There is no third
  option, so the program cannot be written at all.

  This is not hypothetical: `core::range::Bounded` declares `min()`/`max()`
  with no receiver, so `Bounded::min()` is unspellable today even though
  nothing about it is ambiguous.

  A language that can diagnose a conflict must be able to express the
  resolution. That is what this adds.

- **Reasoning:**

  The syntax is `<Type : Spec>::function(args)`, and it reuses two existing
  notations rather than inventing one:

  - `<...>` already means **a type representation**. Applied to an expression
    it casts (`<i32>x`); taken a path from, it selects. Same notation, two
    positions.
  - `:` already means **subject on the left, constraint on the right**
    (`x: i32`, `field: Type`, `T: Ord`, `<T: A + B>`). So `<S : P>` reads "the
    type `S`, viewed through spec `P`" with the rule a reader already knows.

  This is a deliberate improvement on Rust's `<S as P>::make()`, which borrows
  the `as` keyword from casting to mean something unrelated to casting. Omega's
  `:` says the same thing with the separator it already uses for exactly this
  relationship.

  Rust's design is otherwise adopted wholesale, because the three-tier ladder
  is the right shape:

  | Form | Supplies | Resolves when |
  |---|---|---|
  | `S::make()` | the type | unambiguous |
  | `P::make()` | the spec | `Self` inferable from context |
  | `<S : P>::make()` | **both** | **always** |

  Rung 1 works today. Rung 2 works for *instance* functions
  (`Display::fmt(value, out)` — 33 uses in `runtime/`) and is completed here
  for statics. Rung 3 is new. After this plan all three rungs exist for both
  static and instance spec functions, with no gap where a call can be
  diagnosed but not written.

  **Alternatives rejected:**
  - *`<S as P>::make()`* — Rust's spelling. `as` is not a keyword in Omega's
    grammar for this, and `:` already expresses the relation.
  - *`P::make<S>()`* — turbofish-style explicit type arguments. Omega has no
    explicit generic-argument syntax at a call (`lowest<char>()` does not
    parse), so this would invent grammar rather than reuse it.
  - *`S::P::make()`* — reads as a module path, and collides with genuine
    nested paths.

- **Resolved concerns:**
  1. **Operand order.** `<Type : Spec>`, not `<Spec : Type>` — matching `T:
     Ord` and every other use of `:` in the language.
  2. **Coverage.** The form works for **both** static and instance spec
     functions (`<S : P>::method(recv, args)`), so there is one
     fully-qualified spelling rather than one for statics and a different one
     for methods. Instance methods keep `P::method(recv)` as the shorter form.
  3. **`Spec::function(...)` gets `Self` from two different places**, and both
     are supported after this plan:

     - *instance* — from the receiver. Works today, used 33 times in
       `runtime/` (`Display::fmt(value, &mut out)`, `Eq::equals(a, b)`), and
       several of those are inside macro bodies where the receiver's type is
       whatever the caller passed and therefore **unnameable**. Untouched.
     - *static* — from the expected type. Built here, **scoped to the case
       where the declared return type is exactly `Self`**.

     That scope is not a shortcut: it covers every receiverless spec function
     that exists. Measured across the whole tree —
     `Bounded::min()`/`max()` and `Default::default()`, all `=> Self`. The two
     cases left out get an explicit diagnostic rather than a guess:
     - `Self` *nested* in the return type (`=> Option<Self>`) would need
       `unify_generic_type` against the raw return type. The machinery exists;
       nothing needs it yet.
     - a return type not mentioning `Self` at all (`=> usize`) is not
       inferable in principle — `expected` says nothing about `Self`.

  4. **Threading `expected` is additive, not a rewrite.** `Interceptor` and
     `analyze_call` gain the parameter, and `analyze_expr` passes the one it
     already holds at `exprs.rs:80` (the single call site). Four of the five
     interceptors take it and ignore it, so their behaviour is unchanged by
     construction. Only `resolve_spec_qualified_call` reads it.

     This also retires a documented boundary. `docs/06-generics.md` deferred
     the threading with "no example needs this"; two examples now exist, and
     the second one — a generic function whose only type parameter appears in
     its return type being *uncallable*
     (`lowest<T: Bounded>() => T { T::min() }`) — becomes one interceptor away
     once `expected` is in hand. It is **not** fixed here; see `docs/14`.

## Technical Details

### What changes

**`compiler/omega-parser/src/parser/expression.rs`**

`parse_cast` (line 305) currently does: advance `<`, `parse_type`, expect `>`,
then `parse_unary` for the base. It gains a branch after the type:

- next token `>` → today's cast, unchanged
- next token `:` → parse a second type (the spec), expect `>`, then expect
  `::` and an identifier, producing the new qualified-path form

Single-token lookahead after an already-parsed type; no backtracking, and
`<` only begins either form in prefix position, so infix `a < b` is untouched.

**`compiler/omega-parser/src/ast`** — the new form needs somewhere to live.
`ExprPath` (`ast/identifier.rs:97`) already carries `path` plus
`generic_args`/`args_at`; extend it with an optional qualifying pair, or add a
sibling `Expression` variant. Prefer extending `ExprPath`: the resolution this
feeds (`resolve_spec_qualified_call`) already takes its input from
`HirPlaceRoot::Path(expr_path)`, so a qualified path flows through the existing
callee shape rather than needing a parallel path in every consumer.

**`compiler/omega-hir`** — mirror the AST change (`hir.rs`, `lower.rs`).

**`compiler/omega-analyzer/src/analysis/calls.rs` — threading `expected`**

`Interceptor` (line ~19) and `analyze_call` (`exprs.rs:531`) gain an
`Option<&ResolvedType>`; `analyze_expr` passes the one it already has at
`exprs.rs:80`. All five interceptor signatures change identically; four of them
name the parameter `_expected` and ignore it. That is the whole cost of the
threading, and it is behaviour-preserving by construction.

**`compiler/omega-analyzer/src/analysis/calls.rs` — the three call shapes**

`resolve_spec_qualified_call` (line 145) is where all of this lands, and most
of it is reused as-is. Today it: resolves the spec from the path's leading segments,
takes `call.args.first()` as the receiver (line 209), derives `target` from the
receiver's type (line 240), then selects the conformance
(`conformance_for(&target, &spec, &spec_args)`).

The change is to separate **"which conformance"** from **"what is the
receiver"**. Once those are distinct, one code path serves all three shapes,
differing only in where `target` comes from:

| Call shape | `target` from | receiver |
|---|---|---|
| `Spec::fn(recv, …)` | the receiver's type (today) | first argument |
| `<Type : Spec>::fn(…)` | **written in the path** | first argument if the function takes self, else none |
| `Spec::static_fn()` | **`expected`** | none |

For the third, the spec function must be looked up *before* a receiver is
demanded — that lookup is what says whether one is needed. `call.args.first()`
at line 209 is only consulted when the resolved function declares a self mode.
The static branch instead requires the declared return type to be exactly
`Self`, and takes `expected` as the target; a nested-`Self` or `Self`-free
return type is a diagnostic, not a guess (see Resolved concerns).

**`compiler/omega-analyzer/src/analysis/paths.rs`** (line 777) and
`error/kind.rs`/`error/render.rs`

`AmbiguousConformanceStatic` already carries `target` and `function`. It gains
the candidate specs, and its rendering prints the qualified spelling for each:

```
error: conforming static function 'S::make' is ambiguous
  = note: declared by P and Q
  = help: name the one you mean: `<S : P>::make()` or `<S : Q>::make()`
```

This mirrors what the spec-object ambiguity error already does (it names the
declaring specs and prints the narrowing cast). Rust needed two separate issues
to get its E0034 to this standard; Omega should ship at it.

The same treatment applies to `MethodNotInScope` ("method 'less_than' comes
from spec 'Ord' but is not in this bound context"), which should now suggest
`<i32 : Ord>::less_than(a, b)` as the way to call it.

**Docs** — `08-specs.md` (the three-tier ladder, replacing the current
"available through a generic bound, or explicitly as `Animal::make_sound(&dog)`"
sentence), `11-strings-casting-and-slices.md` (the `..<` note, see below),
`14-known-issues.md` (**already written** — the uncallable generic-factory
entry, which also records the static-`Spec::` case and names the missing
`expected` threading as their shared blocker; this plan only needs to keep it
accurate if the diagnostic wording changes).

### What must not change

- **`<Type>expr` casting.** Same grammar, same meaning; the new form is
  distinguished only by the `:`.
- **`Spec::function(receiver, args)`** keeps working for instance methods —
  it stays the shorter spelling.
- **`Type::function(args)`** keeps working when unambiguous.
- **`Spec::function(receiver, args)` for instance methods.** Kept, unchanged,
  and still the shorter spelling — it is the only form that works when the
  receiver's type cannot be named, which is the case in every macro body that
  uses it. Its resolution path must not change when the static case is added
  beside it.
- **The other four interceptors.** They receive `expected` and must ignore it.
  Making `resolve_overloaded_call`/`resolve_generic_call`/the two static
  variants *use* it is a separate change with its own risk; doing it here
  would make this plan unbisectable against a call-resolution regression.
- **The uncallable generic factory.** `lowest<T: Bounded>() => T` stays
  uncallable. `resolve_generic_static_call` could use `expected` once it is
  threaded, but that is the follow-up, not this.
- **Method-call syntax for spec functions** (`10.less_than(20)`) is a separate
  proposal and is not part of this work.

### Risks and open questions

1. **`..<` collision, inherited not introduced.** `docs/11` records that a cast
   immediately after `..<` is already ambiguous (`0..<i32>len` lexes as `0`
   `..<` `i32` `>` `len`). `0..<<S : P>::min()` lands in the same trap with the
   same workaround (bind to a local first). It needs a doc note, not a grammar
   fix — fixing it properly means revisiting `..<` tokenization, which is out
   of scope.
2. **`>` splitting.** `expect_close_angle` and the `pending_gt` machinery exist
   because `>>` lexes as one token. A qualified path ending in a generic spec
   (`<S : P<T>>::make()`) hits exactly that, and must go through the same
   split-aware path rather than a bare `Gt` check.
3. **Where the qualified pair lives in the AST.** Extending `ExprPath` is the
   recommendation, but if that turns out to widen every `ExprPath` consumer
   awkwardly, a dedicated `Expression` variant is acceptable — flag which was
   chosen rather than deciding silently mid-implementation.

## Implementation Plan

1. **Parser only.** Add the `:` branch to `parse_cast` and the AST shape it
   produces; mirror into HIR. Nothing resolves it yet — a parser test asserting
   the shape is the deliverable, and the tree stays green.

2. **Resolution for statics.** Teach `resolve_spec_qualified_call` to take its
   target from the qualified path instead of a receiver when one is written.
   `<S : P>::make()` now works, including the ambiguous case that has no
   spelling today.

3. **Resolution for instance methods.** `<S : P>::method(recv, args)`, reusing
   the existing `adapt_self_argument` receiver handling but with the
   conformance already selected by the written target.

4. **Thread `expected`.** Change `Interceptor`, `analyze_call`, and all five
   interceptor signatures; pass it from `exprs.rs:80`. Four interceptors ignore
   it. No behaviour changes — this step is verifiable purely by the suite
   staying green, and is deliberately separate so a later regression can be
   bisected to it.

5. **`Spec::static_fn()` via `expected`.** Look the spec function up before
   demanding a receiver; when it is receiverless and returns exactly `Self`,
   take `target` from `expected`. `x : char = Bounded::min();` now works.

6. **Diagnostics.** `AmbiguousConformanceStatic` gains its candidate specs and
   prints a qualified spelling per candidate; `MethodNotInScope` suggests the
   qualified form; and the two uninferable static cases (nested `Self`, no
   `Self`, or no `expected` at all) say so and name `<Type : Spec>::fn()` /
   `Type::fn()` instead of reporting a bogus argument count.

7. **Docs**, per the list above.

## Testing

- **`Spec::static_fn()` cases, asserted by execution:**
  `x : char = Bounded::min();` yields `'\u{0}'` and `max()` yields
  `'\u{10FFFF}'`; `x : i32 = Default::default();` yields `0`; the same call
  in argument position (`takes(Bounded::min())`) infers from the parameter
  type; and a `conform`-supplied override still beats a spec default through
  this path.

- **New cases, asserted by execution** (which body ran, not merely that it
  compiled): `<S : P>::make()` and `<S : Q>::make()` select different bodies
  for a type conforming to both; `<i32 : Bounded>::min()` and
  `<i32 : Bounded>::max()` return the domain limits; `<Dog : Animal>::
  make_sound(&dog)` matches what `Animal::make_sound(&dog)` returns; the form
  works with a generic spec (`<S : P<i32>>::make()`), exercising the `>>` split.

- **Negative cases:** `<S : NotASpec>::make()` names the non-spec;
  `<S : P>::make()` where `S` does not conform to `P` reports the missing
  conformance, not a parse error; `<S : P>::nonexistent()` names the spec that
  lacks the function. For statics: `Bounded::min()` with **no** expected type
  (`x := Bounded::min();`) says `Self` cannot be determined and names the two
  working spellings — **not** an argument count; a spec static returning
  something other than `Self` (`spec F { n() => usize; }`, `x : usize =
  F::n();`) is rejected as uninferable rather than silently binding `Self` to
  `usize`. And the diagnostic quality cases: `S::make()` with two
  conformances must print both `<S : P>::make()` and `<S : Q>::make()`.

- **Regression risk:** `parse_cast` is the single highest-risk edit — every
  existing `<Type>expr` in the tree flows through it, and `runtime/core` and
  `runtime/std` are cast-heavy (`<Self>0`, `<u8>cp`, `<*[]u8>self`). The parser
  test suite plus a clean `just build-core`/`build-std` is the guard.
  `compiler/omega-driver/tests/conform.rs` (59 tests) covers the conformance
  selection this reuses.

- **Gates:** `cargo test`, then `just test-io test-stdio-contract test-core-only
  test-root-layout test-allocator-only test-multi-print test-range test-char
  test-spec-dispatch run-exec`. Symbol tables should be unchanged — this adds a
  spelling, not a conformance.
