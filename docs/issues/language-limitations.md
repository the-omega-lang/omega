# Language limitations and caveats

These notes were migrated out of normative language chapters. They describe current limitations, deliberate restrictions that are easy to mistake for bugs, or implementation gaps. Resolved historical notes are intentionally omitted.


## Functions

Normative chapter: [`../language/functions.md`](../language/functions.md)

- A deeper `module::Type::function(...)` static-call path (through more
  than one level of module qualification) resolves without overload
  disambiguation at all — a documented, narrow gap distinct from the
  ordinary locally-visible-type overload path described above.


## Primitives & representation

Normative chapter: [`../language/types-and-primitives.md`](../language/types-and-primitives.md)

- **No real C-ABI aggregate-passing convention.** Structs/enums are passed
  as flattened positional scalars, not per platform calling-convention
  aggregate rules. This works fine for Omega-to-Omega calls (including
  across separately-compiled `--import` object files, since both sides
  agree by construction) but means an Omega function taking a struct
  by value is not safely callable from, say, hand-written C expecting the
  System V ABI's actual struct-passing rules.
- **`isize`/`usize` width is target-dependent by design** — nothing in
  `core` bakes in a `min_value`/`max_value` bound for them, since any
  literal bound would silently be wrong on a target this toolchain wasn't
  built assuming.


## Control flow

Normative chapter: [`../language/control-flow-and-operators.md`](../language/control-flow-and-operators.md)

- `char` has comparison (`== != < <= > >=`) and can be used as a `match`
  scrutinee, including ranges (`'A'..='Z'`) — see
  [primitives](../language/types-and-primitives.md) and
  [enums & pattern matching](../language/enums-and-pattern-matching.md). Arithmetic
  and bitwise ops are rejected; cast explicitly to `u32` when codepoint
  arithmetic is intended — see [primitives](../language/types-and-primitives.md)'s "`char`,
  `bool`, and pointer arithmetic" section.
- Binary-op literal narrowing is **earliest-wins, not most-specific-wins**
  — matching the identical, already-accepted trade-off `if`-expression
  branches make (`if true { 8 } else { 7u16 }` doesn't retroactively
  narrow branch 0 either). `0 < some_i64_var` (the literal written first,
  the concretely-typed operand second) still won't narrow — write
  `some_i64_var > 0` or cast explicitly instead. Not a gap left over from
  the fix above; a deliberate scope match with existing precedent.


## Structs & unions

Normative chapter: [`../language/structs-and-unions.md`](../language/structs-and-unions.md)

None specific to structs/unions themselves — see
[generics](../language/generics.md) for the remaining generic-instantiation gaps
(spec-to-spec generic forwarding, literal narrowing), neither of which is
struct/union-specific.


## Enums & pattern matching

Normative chapter: [`../language/enums-and-pattern-matching.md`](../language/enums-and-pattern-matching.md)

- **`match` scrutinee unification is not part of literal-inference** — an
  arm-body's own type isn't coerced against a match's other arms the way
  `if`-branches are; this was deliberately excluded from the literal-
  inference feature (judged too entangled with exhaustiveness/refinement to
  fold in safely at the time).
- A float scrutinee is explicitly unsupported in `match`
  (`UnsupportedMatchScrutinee`), not silently mishandled — `char` is
  supported (see above).


## Visibility

Normative chapter: [`../language/visibility.md`](../language/visibility.md)

- **`import reveal` is not a re-export.** `import reveal lib::x;` only
  lets *this* module's own references bypass `x`'s visibility — it doesn't
  change what a third module sees. Deliberate re-export is `alias`'s job: an
  `exposed alias Public = lib::x;` makes `x` reachable from outside `lib` as
  `Public`, without changing `x`'s own visibility. See
  [`aliases.md`](../language/aliases.md).
- **A named import alias's overload candidate set is fixed at import
  time**, deliberately not reachable by a later call-site `reveal`: `import
  lib::pick;` (no `reveal`) permanently excludes any overload of `pick`
  this module can't see from the candidate set — a call whose arguments
  only match an excluded overload is a hard `NoMatchingOverload`, as if
  that overload didn't exist. Only `import reveal lib::pick;` brings every
  overload into context (with no call-site `reveal` needed afterward). A
  module-qualified reference through a *whole*-module import (`lib::pick(...)`
  via plain `import lib;`) is explicitly exempt from this restriction —
  every overload is always a candidate there, and call-site `reveal` still
  works normally, since there's no per-symbol "import reveal" granularity
  that could even apply to a whole-module import.
- Build reproducibility (several `HashMap` iteration sites making object
  files differ build-to-build for identical source) was discovered
  incidentally while verifying a visibility change, but is unrelated to
  visibility itself and has since been fixed — see
  [modules & linkage](../language/modules-and-imports.md).

Macros are ordinary visibility-bearing items: an unmodified macro is
file-local, `shared macro` is package-visible, and `exposed macro` is
visible to importers and the ambient `core` prelude.


## Specs

Normative chapter: [`../language/specs-and-conformance.md`](../language/specs-and-conformance.md)

- **A vtable's real cache/dedup key is its own resolved slot list
  (`Analyzer::type_implements_spec`'s output, one concrete method's
  `decl_id` per slot), not `(concrete type, spec, spec type args)`
  directly.** The two coincide almost always, but the slot list is
  strictly more precise: two coercions that happen to resolve to the
  identical ordered method list always produce byte-identical vtables no
  matter which concrete type or spec they came from, so sharing one copy
  is correct even then. The *symbol name* still has to be a function of
  `(concrete, spec, spec type args)` though (`decl_id`s aren't meaningful
  across separately-compiled translation units) — see
  `mangle::vtable_symbol`.
- **Only `core` can add inherent methods to primitives.** Any package allowed
  by the orphan rule can conform a concrete target to a spec.
- **Variadic spec functions are not planned**, not a limitation. `f(*self,
  ...)` is rejected at the spec's own declaration
  (`VariadicSpecFunctionUnsatisfiable`): Omega has no ordinary Omega-convention
  variadic function *definitions* — only `foreign` declarations, under a
  convention that supports variadics (`c`; `sysv64` on its supported
  targets), may be variadic — so no `conform` block or spec default could
  ever supply a matching body, and nothing else in the language is scheduled
  to support `...`. Not banned forever, just unscheduled; the `is_variadic`
  plumbing behind the guard is complete, and the guard lifts the day variadic
  definitions exist.
- **A definition-site `spec T` return type is rejected** — the same
  `SpecStaticNotAllowedHere` on a free function and a method alike (see
  "Return position, on an ordinary (non-spec) function" above). A conform
  method satisfying a `=> spec Bound<...>` requirement declares its own
  *concrete* return type (`std::list`'s `to_iterator(*self) =>
  ListIterator<T>`), which is checked against the bound.
- **Conformance proving is goal-directed.** Proving `T: Spec` instantiates
  only the blanket/generic templates that can produce *that* spec, never
  every template matching the type; each proof pulls in precisely the
  templates it needs, so a chain of blanket derivations
  (`conform S to A`, `conform<T: A> T to B`, `conform<T: B> T to C`)
  resolves in any declaration order, and a cycle is reported only when the
  goal stack closes on itself — with the chain that closes it. A bound
  *context* (conjunction members, entailed derived conformances) is
  body-checking information, computed when a body is checked, never in the
  middle of a proof.
- **A conjunction bound's member order is interchangeable everywhere.**
  `T: A + B` and `T: B + A` describe the same set, because both canonicalize
  to the same resolved shape: they compare equal in blanket precedence (so
  the two spellings of the same blanket are a `DuplicateConformance`), each
  entails the other's derived conformances in a bound context, and a generic
  conjunction (`Iter<T> + Eq` vs. `Eq + Iter<T>`) expands identically with
  its arguments substituted.
- Generic primitive and conform templates are instantiated lazily for the
  concrete target types a compilation uses.


## Strings, casting & slices

Normative chapter: [`../language/strings-casts-arrays-and-slices.md`](../language/strings-casts-arrays-and-slices.md)

- **`*str` is not actually guaranteed valid UTF-8.** The cast family
  treats `Str` and `Slice{U8|I8}` as fully interchangeable in *both*
  directions with no validation — `<*str>some_arbitrary_byte_slice`
  compiles today and freely relabels arbitrary bytes as `*str`, including
  invalid UTF-8. This is a known, explicitly deferred inconsistency (the
  original design intent was an *asymmetric* rule — `*str → *[]u8` free,
  `*[]u8 → *str` fallible, mirroring Rust's `str::from_utf8` — but the
  shipped implementation is symmetric). Deliberately deferred by the user
  ("after I implement core, we'll handle that") pending a real
  UTF-8-validating conversion function in `core`; string *literals*
  themselves are still always valid UTF-8 by construction (parsed from a
  UTF-8 source file).
- `const_eval` (compile-time constant evaluation, used for enum header
  fields) does not support casts at all — any header field that would have
  needed a cast (e.g. `*u8` text with a `\0`) had to migrate to `*str`
  instead, since there was no alternative. Body/dynamic fields, struct
  fields, and ordinary expressions have no such restriction.
- `&[...]`/bare-array compile-time construction is deliberately **not
  deduplicated** across occurrences the way string/byte-string data is —
  `ConstValue` isn't cheaply hashable (nests, and floats have no total
  order), and each occurrence is a one-shot construction site, not
  plausibly repeated the way string literals are. A documented
  simplification, not an oversight.
- A cast immediately after `..<` is genuinely ambiguous with the operator
  itself (`0..<i32>len` lexes as `0` `..<` `i32` `>` `len`, not `0` `..`
  `<i32>len`) — bind the cast to a local first (`n := <i32>len;
  &ptr[0..<n]`) rather than writing it inline in the range. `..=`/`..`
  don't share this ambiguity (neither ends in a bare `<`), so `0..=<i32>len`
  parses as written. A fully-qualified spec call
  (`0..<<S : P>::min()`) lands in the same trap, with the same
  workaround -- bind to a local first.


## `for` .. `in` loops

Normative chapter: [`../language/iteration-and-ranges.md`](../language/iteration-and-ranges.md)

- **`*str`/`*[]T` don't implement `ToIterator` yet.** `for c in
  some_str { }` needs a hand-written wrapper struct today (as in the
  example above). Wiring the built-ins up is a natural follow-up using the
  the same generic conform mechanism collections use (see
  [specs](../language/specs-and-conformance.md)) — not done as part of this
  feature, to keep its own scope to the language mechanism and the two
  specs it depends on.
- **`Option<T>` itself has no convenience methods** (`is_some`,
  `unwrap_or`, ...) — see [core library](../guide/core-library.md).
- **A type implementing `ToIterator<T>` more than once, at different `T`,
  has no way to disambiguate which `for x in y` picks** — unlike an
  ordinary overloaded method call, there's no argument shape to resolve
  against (`to_iterator(*self)` takes none), and the explicit-cast
  disambiguation the dynamic-dispatch design used to offer
  (`<*spec ToIterator<u64>>expr`) no longer applies now that `ToIterator<T>`
  isn't object-safe. Narrow in practice (this scenario needs two
  `to_iterator` overloads differing only in return type, which most specs
  won't hit), but a genuine, currently-unsolved gap if it comes up.


## Zero-sized types (`marker`)

Normative chapter: [`../language/marker-types.md`](../language/marker-types.md)

- The combined per-function stack frame (see "Addresses" above) sizes
  itself off every non-parameter local's declared type, whether or not
  that local is ever actually read — unlike the old one-slot-per-local
  model, where a genuinely unused local cost nothing (no slot was ever
  allocated for it). A legitimately dead local now occupies space in the
  frame regardless; the dead-code lint already warns about unused
  variables, so this is expected to be rare in practice, not a
  correctness concern.
- The `ZeroSizedAggregate` diagnostic for a generic struct/union that
  only becomes zero-sized for one instantiation points at the generic
  declaration's own span, not the specific instantiation call site that
  triggered it — consistent with how every other `signature_of_struct`/
  `signature_of_union` check in this compiler anchors its error, but
  worth knowing if the message looks like it's pointing at "healthy"
  code when only one particular type argument is actually the problem.

## Macros

Normative chapter: [`../language/macros.md`](../language/macros.md)

- **A macro parameter cannot be optional.** The only way to accept "zero or
  one argument" is a trailing variadic parameter plus a compile-time guard
  that rejects a second one. `core::panic::panic$` does exactly this, so
  `panic$("a", "b")` reports a redeclaration of the guard binding
  (`PANIC_TAKES_AT_MOST_ONE_MESSAGE`) rather than an arity error naming the
  macro. The rejection is deliberate and reliable; only the diagnostic is
  worse than it would be with real optional parameters.


## The standard library

Guide: [`../guide/standard-library.md`](../guide/standard-library.md)

- Forgetting an owning value's `.free()` leaks it; there is no RAII or leak
  detector.
- `Result<T, E>` does not exist. The APIs use `Option<T>`, `bool`, or
  out-parameters where appropriate.
- The built-in formatter is allocation-free but float formatting is
  fixed-precision rather than shortest-round-trip.

## `defer` is not allowed inside loops or another `defer`

Current semantic analysis rejects a `defer` whose lexical context is inside any loop, and rejects a deferred body containing another `defer`. The language documentation describes the function-scoped FILO behavior of supported defers; lifting these restrictions requires explicit control-flow/lowering support rather than merely relaxing syntax.
