# No `never` arrays or `never` pointers: nested value positions and divergence propagation

## Task Description

- **Deliverable:** `never` is rejected in every position that stores a value,
  including nested ones — array element, pointer pointee, slice item,
  unknown-size array item, and a function *type*'s parameter — while a pointer
  to a never-returning function stays legal. Divergence propagates through
  operator and cast expressions instead of being rejected by them. The
  `known-issues.md` entry shrinks to only what genuinely remains.

- **Purpose:** `docs/language/types-and-primitives.md:20` already states the
  rule — *"`never` is permitted only as a function-like declaration's return
  type"* — and repeats it as *"`never` is not a storable value type"*. Today the
  compiler only inspects the **outer** type spelling, so `a: never` is rejected
  while `a: [1]never`, `a: *never` and `a: *[]never` compile with no diagnostic
  at all. Nothing here is a language change: the specification is already
  normative and the implementation is the stale side.

- **Chosen direction:**
  1. **Gate at each composite construction site, not with a recursive
     predicate.** `resolve_anonymous_enum_type`
     (`compiler/omega-analyzer/src/context.rs:495-510`) already does exactly
     this for its members, with the reasoning stated in place: *"A member is
     stored inline, exactly like an aggregate field, so it faces the same
     value-type restrictions."* Generalize that to every other composite the
     resolver builds. A recursive `contains_never` over `ResolvedType` is
     **wrong** and must not be written: it would reject `*() => never`, a
     pointer to a never-returning function, which the `never` chapter
     explicitly blesses (*"a `never` returned by a gap function, a function
     pointer, or a spec's dynamic call is the same contract"*). Construction-site
     gating distinguishes the two structurally, because a function type's
     return is simply not one of the gated sites.
  2. **Three entry paths, one rule — say so rather than collapsing them.**
     After this change the non-storable rule is enforced in three places, and
     they are not duplicates: `allow_never` (`analysis/mod.rs:1333,1362`) owns
     the **outer written spelling** (`a: never`), the new construction-site
     gates own **nested written positions** (`a: [1]never`), and the
     inferred-binding gate (`analysis/stmts.rs:165`) owns the case where **no
     type was written at all** (`x := stop()`). Each covers a path the others
     cannot see. Record that division in a short comment at the gate helper so
     a later reader does not "simplify" one of them away.
  3. **Divergence propagates through operators and casts.** If any operand of a
     binary operator, a unary operator, or a cast diverges, the whole expression
     is typed `never`, and operator applicability / cast legality are not
     checked against the diverging operand. Two precedents in the tree already
     have this shape: `<void>` casts deliberately stay divergent
     (`analysis/exprs/operators.rs:220-227`, *"stays divergent when the operand
     cannot complete"*), and `analyze_if` yields `Never` when every arm
     diverges. The operands must stay in the checked tree so their effects and
     ordering still lower.

- **Part C dissolves — do not "fix" it.** The brief for this task carried a
  third item, that inferred bindings can still store `void` (`x := consume(1)`).
  Checking the specification first: `docs/language/types-and-primitives.md:19`
  says *"`void` is a zero-sized value type."* Binding one is therefore **legal
  and correct**, not a gap. The sentence claiming otherwise in the
  `known-issues.md` entry is simply wrong and is deleted in step 8 rather than
  carried forward as deferred work. No code change.

- **Rejected alternatives:**
  - *Recursive `contains_never` / `is_storable` over `ResolvedType`* — rejected
    above; it cannot express the function-return exemption without
    reintroducing position awareness, at which point it is the construction-site
    gate with extra steps.
  - *Special-casing `never` inside each operator's applicability check* —
    multiplies the rule across `analyze_binary_op`, three unary funnels and six
    `InvalidCast` sites. One early check per funnel is the same rule stated once.

## Technical Details

- **Initial context boundary:** `compiler/omega-analyzer/src/context.rs`
  (type resolution), `compiler/omega-analyzer/src/analysis/exprs/operators.rs`
  (operators and casts), and `docs/language/types-and-primitives.md#never`.
  `omega-mir` is opened only for the verification in step 6; `omega-codegen`,
  `omega-parser` and `omega-hir` stay closed.

- **Verified current behaviour** (probed against `target/debug/omgc` at
  `8307dfa`; confirm, do not re-derive):

  | Spelling | Today | Wanted |
  |---|---|---|
  | `a: never` | rejected | rejected (unchanged) |
  | `a: [1]never` | **accepted, no diagnostic** | rejected |
  | `a: [1][1]never` | **accepted** | rejected |
  | `a: *never` | **accepted** | rejected |
  | `a: *[]never` | **accepted** | rejected |
  | `a: *[?]never` | **accepted** | rejected |
  | `f: *(a: never) => void` | **accepted** | rejected |
  | `f: *() => never` | accepted | **accepted (must not regress)** |
  | `B<never>`, `enum i32 \| never` | rejected | rejected (unchanged) |
  | `1 + stop()` | `InvalidBinaryOperand` | type `never` |
  | `<i32>stop()` | `InvalidCast` | type `never` |

  Note the self-contradiction being closed: a `never` parameter on a real
  declaration is rejected, but the same parameter inside a function *type* is
  not.

- **Affected files/symbols:**

  *`context.rs` — gate these composite construction sites:*
  `SizedArray` item (`:469-473`); `Slice` item (`~:957`); `Array` item
  (`~:970`); `Pointer` pointee, both the `Self` arm (`~:997`) and the general
  arm (`~:1008`); function-type parameters in `resolve_function_type`
  (`:294-310`). **Exempt:** `resolve_function_type`'s return type (`:320`) —
  the one position where `never` is legal. Already gated, leave as the model:
  `resolve_anonymous_enum_type` (`:506`).

  `resolve_function_type` (`:286`) has exactly one caller — the `Type::Function`
  arm of `resolve_type` (`:460`) — so gating its parameters affects function
  *types* only and cannot double-report on a real declaration, whose parameters
  are gated by `allow_never` on a different path.

  The gate helper should reject `ResolvedType::Never` only. Do not fold the
  `Spec` rejection into it: `resolve_anonymous_enum_type` rejects specs as
  members, but a pointer to a spec is the legitimate spec-object form and must
  keep resolving.

  *`analysis/exprs/operators.rs` — divergence propagation funnels:*
  `analyze_binary_op` (`:552`), before the char and pointer-arithmetic checks;
  `analyze_negate` (`:4`); `analyze_not` (`:48`); the bit-not check (`~:160-180`);
  and `analyze_cast` immediately beside the existing `<void>` case (`~:218-227`),
  which is positioned before all six `InvalidCast` sites (`:287, 370, 394, 439,
  456, 467`) and so covers them in one place.

- **Interfaces/invariants:**
  - `*() => never` resolves, and calling through it still reaches the
    `NeverCallReturned` runtime guard. This is the single most important thing
    not to regress; it is a real contract with foreign/indirect code, not a
    curiosity.
  - Diverging operands stay in the checked tree. Typing the expression `never`
    must not discard operands, or `a() + stop()` would stop running `a()`.
  - The three enforcement paths in "Chosen direction" item 2 each remain
    reachable; none is redundant.
  - `void` remains a storable zero-sized value type.

- **Out of scope:** `loop` is parsed only as a statement, so
  `consume(loop { })` is a parse error. Making `loop` an expression is a
  grammar/language change needing its own decision, not a `never` bug — leave
  that sentence in the `known-issues.md` entry.

- **Risks/open questions — escalate rather than decide alone:**
  - If gating a composite site breaks `runtime/core`, `runtime/std` or
    `runtime/plat`, stop and report. A real `never` array or `never` pointer in
    the runtime would mean the design constraint conflicts with existing code,
    which is the user's call, not a licence to edit the runtime around it.
  - If typing a `BinaryOp`/`Cast` node `never` causes trouble in MIR lowering or
    codegen — a `Never`-typed node in a position that previously could not hold
    one — stop and report rather than adding a backend workaround. Step 6 is
    where this surfaces.

## Implementation Plan

1. Add the value-position gate helper to `context.rs` beside
   `resolve_anonymous_enum_type`, rejecting `ResolvedType::Never` with
   `TypeResolutionError::NeverNotAllowedHere`, with a comment recording the
   outer/nested/inferred division of responsibility. Route
   `resolve_anonymous_enum_type`'s existing `Never` arm through it so there is
   one implementation.
2. Apply it to the `SizedArray` item and both `Pointer` pointee arms. Nested
   spellings like `[1][1]never` fall out, because each level is constructed
   through a gated site.
3. Apply it to the `Slice` and `Array` item sites.
4. Apply it to `resolve_function_type`'s parameters, leaving the return type
   ungated. Verify `*() => never` still resolves before moving on.
5. Add the divergence early-return to `analyze_binary_op`, the three unary
   funnels, and `analyze_cast` beside the `<void>` case.
6. Verify the MIR consequence of step 5 rather than assuming it:
   `lower_sequence` (`omega-mir/src/lower/function/expr.rs:324`) documents that
   it covers call, binary and aggregate operands, so `a() + stop()` must run
   `a()` and `stop() + a()` must not run `a()`. Confirm a `Never`-typed
   `BinaryOp`/`Cast` node lowers without tripping the `push_stmt`/`terminate`
   asserts (`lower/function.rs:250,258`).
7. Tests (see below).
8. Docs. `docs/language/types-and-primitives.md` needs **no normative change** —
   lines 19-20 and the `never` section already state both rules. Consider one
   clarifying addition only if the enumeration *"invalid as a local/field/
   parameter type, generic argument, or aggregate member type"* reads as
   exhaustive; if so, extend it to name array element, pointer pointee and
   slice item positions and state the function-return exemption. Then rewrite
   the `known-issues.md` entry (currently at `:431`) to keep only the `loop`
   sentence, deleting the `[1]never`, binary-operand, cast and `void`-binding
   sentences — the last because it was never a defect.

## Testing

- **New/changed cases** — root conformance under `tests/`, neighbouring the
  `t44`/`t46` families:
  - Positive: `*() => never` declared, assigned a never-returning function, and
    called — proving the exemption survives and the call still reaches the
    `NeverCallReturned` guard when the callee returns. This is the regression
    that matters most.
  - Positive: `a() + stop()` runs `a()` before diverging; `stop() + a()` does
    not run `a()`. Prove both orders with printed markers, as
    `t44_never_call_returned` already does for call arguments.
  - Positive: `<i32>stop()` and `-stop()` compile and diverge.
- **Specification trace:** `docs/language/types-and-primitives.md:20` (*"`never`
  is permitted only as a function-like declaration's return type"*), the
  `never` section's *"not a storable value type"* sentence and its function-pointer
  contract sentence, and its general *"A diverging expression is compatible with
  any expected expression type"* rule for the operator/cast cases.
- **Negative/diagnostic cases** — one per wrongly-accepted spelling in the table:
  `a: [1]never`, `a: [1][1]never`, `a: *never`, `a: *[]never`, `a: *[?]never`,
  `f: *(a: never) => void`. Each must assert the **exact** expected diagnostic
  via the ordinary `expected.stderr`: when compilation fails, `bin/test-runner`
  compares the compiler's stderr against that file, which 43 existing cases
  rely on. Do **not** use `expected.compiler.stderr`, which asserts stderr after
  a *successful* compile. Use a local (`a: [1]never;`) rather than a struct
  field as the subject — a field spelling errors incidentally as *"struct 'S'
  has no sized fields"* before the never gate is reached, which would make the
  case pass for the wrong reason.
- **Regression coverage:** the whole `t44`/`t46` families, which cover the
  divergence rules established by the previous task; `cargo test --workspace`
  for the analyzer's own type-resolution tests.
- **Commands/target coverage:** `./bin/test-runner <case>` while iterating, then
  `just test-all` and `cargo test --workspace`. `just test-all` builds `core`,
  `std` and `plat`, so a `never` composite anywhere in the runtime surfaces
  immediately — that build is the real check on the first risk above.
