# Design debt

Unresolved design/architecture inconsistencies migrated from the former monolithic design-review document. Resolved review findings are intentionally omitted.

### Enum-variant matching and enum-tag matching are two unrelated exhaustiveness engines with very different practical requirements

The docs describe one unified exhaustiveness mechanism (`exhaustiveness.rs`)
covering "enums, integers, `bool`, and `char`." In truth `analyze_enum_match`
(matching the enum value itself, `analysis.rs:5355-5476`) never touches `
exhaustiveness.rs` — it just tracks a `HashMap\<usize, Span>` of covered variant
indices and requires exactly one arm per variant, however many variants there
are. `analyze_value_match` (matching an integer/bool/char scrutinee, `
analysis.rs:5544-5575`) is the one that actually runs the interval-sweep
exhaustiveness checker over the scrutinee type's full domain. Since an enum's `
.tag` field is an ordinary integer, matching *on the tag* goes through the
second path, not the first:

```
enum E(tag: u32) { First(10), Second(20) }

match e { E::First => .., E::Second => .. }             # exhaustive, no `else` needed

match e.tag { 10 => .., 20 => .. }                        # `else` REQUIRED —
                                                             # exhaustiveness checker
                                                             # has no idea only 10/20
                                                             # ever occur
```
Both behaviors are individually correct for what each function actually knows,
but a user has no way to predict which one they'll get just by looking at "am I
matching an enum-shaped thing" — the moment you reach for `.tag` instead of the
value itself (plausible whenever you want the numeric tag for
logging/serialization/FFI alongside a match), exhaustiveness checking gets
categorically weaker with no warning that anything changed.

### Packed-by-default layout's safety argument is single-target, but `\--target` already offers a second target

`total_bytes`/the packed-layout doc comment (`compiler/omega-codegen/src/lib.rs:397-405`)
justifies "packed by default, no implicit alignment" as safe with: "x86_64
tolerates unaligned loads/stores with no correctness issue, so packed is safe
as a default." That's a real, correct fact about x86_64. But `
compiler/omega-codegen/src/target.rs` already defines `Arch::Aarch64` as a genuine,
CLI-selectable `\--target` option (`omgc ... --target=aarch64-linux`) — and
AArch64 does not give the same blanket guarantee: exclusive/atomic load-store
instructions fault on misalignment, several OS/embedded configurations enable
strict alignment-fault checking globally, and certain SIMD load/store forms
require natural alignment. The safety argument was written for (and is only
actually true on) one of the two architectures the compiler already advertises
supporting. Nothing has actually broken yet — nothing in this codegen currently
emits exclusive/atomic/SIMD instructions — but the written justification for a
default that touches every struct/enum layout in the language no longer matches
the compiler's own stated target surface, and nothing re-derives or gates it
per-target.

### `cast_class`/`numeric_kind` hardcode pointer width to 64 while codegen's actual pointer type is genuinely target-dependent

Related to the above, and to the already-documented "single-target- assumption"
note on `isize`/`usize` in `../language/types-and-primitives.md`: `cast_class` and `numeric_kind` (`
resolved_type.rs:640-712`) hardcode `ISize`/`USize`/ `Pointer` to width 64 for
every cast-kind decision (this is what decides `Reinterpret` vs. `IntExtend`/`
IntTruncate`). Codegen's real IR type for these — `codegen.pointer_type()` via `
target_config()` — genuinely varies by target. Today this is unreachable in
practice: both currently-supported architectures (`X86_64`, `Aarch64`) are
64-bit, so the hardcoded value and the real one always agree. But the moment a
32-bit target is ever added (nothing structurally prevents `Target::Arch` from
growing one — the CLI already treats `\--target` as a real, user-facing axis), a
cast like `\<u64>some_usize` would still resolve to `CastKind::Reinterpret`
(both "width 64" per the hardcoded table) while the real IR leaves involved
would be I32 vs. I64 — a case `Reinterpret`'s own contract ("same IR
representation already, no instruction needed") cannot actually satisfy,
producing either a Cranelift verifier failure or a silent miscompilation
depending on how it fails. Worth fixing before a 32-bit target is added, not
after — this is the kind of latent assumption that's cheap to fix now and
expensive to debug once it's live.

### `reveal reveal x` is accepted by the parser and always produces a spurious `UnnecessaryReveal` warning

`parse_unary` recurses freely on its own prefix set with no special-casing for a
doubled `reveal`, so `reveal reveal x` lowers to a nested `Reveal(Reveal(x))`
with two stacked bypass frames. `check_visibility`/ `check_member_visibility`
only ever mark the *innermost* active frame as used, so if the bypass is
genuinely needed and gets consumed while evaluating `x`, the *outer* `reveal`'s
frame is never marked used and always reports `UnnecessaryReveal` — even when
removing it would break the inner one's own reasoning about redundancy. Not a
correctness bug (access is still correctly granted either way) and not
high-value to fix on its own, but it's a real, guaranteed false-positive
diagnostic for syntax the parser could just as easily reject outright.

### Implicit enum tag auto-assignment has no width bound-check, unlike explicit tags

An explicit tag value goes through `const_eval`/`const_number`, which
range-checks against the tag's declared width. The implicit-tag path (used when
no `tag: T` is declared at all) just does `NumberValue::Unsigned(declared_index
as u64)` against a hardcoded default width of `U16`, with no equivalent check.
In practice this needs upward of 65536 variants on one enum to matter at all —
squarely theoretical — but it's the one place the "every variant gets a
provably-unique tag" guarantee this compiler otherwise takes seriously (see the
confirmed-sound tag- uniqueness check elsewhere in this same code) isn't
actually backed by a check, just by an assumption in a code comment about
what's "far past any real declaration."

### Overloading is a second, parallel item pipeline that exists only because the query key can't name a candidate

The driver's whole design is "one memoized query per item", keyed by
`ItemKey { module, name, type_args }`. An overloaded name breaks that key:
two candidates share a module and a name, so the key can only ever address
the first-declared one. Everything overloading needs today exists to route
around that single fact:

- its own signature cache and its own body cache, both keyed by
  `(module, declaration index)` instead of by `ItemKey`;
- its own duplicate-declaration check (`check_overload_duplicates`), separate
  from the one the module index already does for every other name;
- its own `ModuleResolver` method (`function_overload_signatures`), plus a
  second one (`raw_import_absolute_path`) that exists *only* so an import
  alias to an overloaded name isn't eagerly collapsed to one winner;
- an explicit skip in **both** whole-program sweeps, and a separate sweep
  right after each of them.

Every candidate is also forced to be non-generic, so `f<T>(x: T)` and
`f(x: i32)` cannot coexist — not a decision anyone made, just what falls out
of the parallel path not having a `type_args` dimension.

**Confirmed empirically, and worse than a clean rejection**: declaring both
(e.g. `free(ptr: *u8) => void { ... }` and `free<T>(ptr: *T) => void { ...
}` in the same module, no call site needed at all) doesn't compile, but
also doesn't report *why* — the only diagnostic is

```
error: cannot use 'main::free' because of its own error
= note: `free`'s own error is reported where it is defined
```

with no other error printed anywhere. `ResolveError::ItemFailed` is meant
to suppress a confusing *secondary* error once the real one has already
been reported elsewhere — here it fires with no primary diagnostic ever
having been shown. The likely mechanism: `ensure_overload_signature`
(`omega-driver/src/bodies.rs`) resolves every overload candidate's
signature for `check_overload_duplicates`'s comparison via
`collect_function_signature(f, None)` with an **empty substitution list**
(`&[]`) — fine for a non-generic candidate, but for a generic one this
means resolving its own still-unbound type parameter (`T` in `*T`) with
nothing bound to it at all, which is exactly the shape "collect this
generic's signature outside of any instantiation" was never designed to
handle. Whatever fails during that resolution attempt isn't surfacing as
its own visible diagnostic before `ItemFailed` wraps it. Not yet root-caused
to the exact line; flagged here for whoever picks up the fix above, since
it means the *symptom* of this gap is a broken error message, not just a
missing feature.

The fix is to make the key able to name a candidate (a disambiguator
alongside `name`, its declaration position, `0` for the overwhelmingly common
unambiguous case), after which both parallel caches, both extra sweeps, and
at least one trait method collapse into the ordinary path — and generic
overloads become possible rather than structurally excluded. Breaking:
changes the resolver trait's surface and every cache key shape.

### `ResolveError::Cycle` carries a chain it never populates

The variant is `Cycle(Vec<Vec<Ident>>)` — a list of module paths, rendered as
`a -> b -> a`, and the diagnostic's own label says "this import completes the
cycle". Both construction sites pass exactly one module, because the query
state is a *map* of `InProgress` markers with no ordering, so neither site can
reconstruct the chain. What actually prints is `cyclic module dependency: a`.

Either populate it for real (keep an in-progress *stack* alongside the state
map; the cycle is then the suffix from wherever the offending key first
appears, and the diagnostic becomes genuinely Rust-quality: `a::X -> b::Y ->
a::X`) or collapse the variant to a single module and reword the message so it
stops implying a chain. The variant is also close to unreachable today — a
by-value type cycle is caught earlier and more precisely by
`RecursiveTypeWithoutIndirection` — which is exactly why the gap has gone
unnoticed.

### Module paths and item paths are the same untyped `Vec<Ident>`

Everything module-shaped is `Vec<Ident>`: cloned per lookup, hashed per query,
carried in every cache key and every diagnostic. It is also structurally
identical to an *item* path — several functions take an "absolute path" that is
really module + item and immediately `split_last()` it, and nothing in the type
system stops the two from being confused (the one place they genuinely differ,
a root's declared name vs. its real on-disk stem, needed a dedicated
translation step and a doc comment warning every other cache to key off the
declared one).

An interned `ModulePathId` plus a distinct `ItemPath` type would make the
confusion unrepresentable and cut the cloning. Breaking across crates: the
`ModuleResolver` trait speaks `&[Ident]` in every method.

### Diagnostic scoping for borrowed modules is three ad-hoc lists

Which findings surface depends on which of three lists a module lands in, with
four different outcomes: errors from a local module surface; errors from an
extern module are dropped; errors from `core`'s tree surface (it's added to
the error scope explicitly, so a broken primitive block still reports); warnings
from `core` are dropped (deliberately — its unused imports shouldn't leak into
every downstream build); warnings from other externs never exist because their
bodies are never checked.

Each individual choice is defensible and documented at its site, but there is
no single stated policy, so the next kind of borrowed module has no rule to
follow. The underlying distinction is ownership: a module is either *compiled*
by this invocation or merely *scanned* for it, and a scanned module should
report exactly the findings that are about something this invocation asked for.
Stating that once, and deriving the scopes from it, replaces all three lists.

### A node's identity is two parameters everywhere, threaded by hand

`(HirId, Span)` is passed as a pair through roughly sixty signatures, most of
which do nothing with it but forward it to the next call and eventually to
`AnalysisError::new`. It is the single biggest reason functions here look
wider than they are, and the reason `clippy::too_many_arguments` is allowed
crate-wide.

Collapsing the pair into one small `Copy` type (a `NodeRef`/`Site`) would cut
two parameters to one across the crate and make "which id goes with which
span" unrepresentable rather than a convention. It is a breaking change to
`omega-hir` (every node would want to hand one out) and touches every call
site in analysis, which is why it wasn't folded into a refactor pass.

### `reveal` still has no backstop for the "every position must remember" invariant

The `&reveal base[range]` bug fixed above was the *third* occurrence of one
pattern: `reveal` is a wrapper the parser can leave in several different
places, and each write/borrow position has to individually remember to strip
it and re-activate the bypass. Three positions have now been fixed one at a
time (`=`, `&`/`&mut` on a plain place, and now the slice/array-literal
operand forms).

The invariant itself is unenforced -- a fourth position added later will
silently drop the bypass again, with no diagnostic pointing at the cause
(since no frame is pushed, `UnnecessaryReveal` cannot fire either). Making
the *place resolver itself* own the bypass, rather than every syntactic
operand position, is the structural fix.

### `Span` cannot identify its source file, which leaks into macros and cross-file diagnostics

`omega-diagnostics::Span` is only `{ start, end }`. The driver separately knows which
`SourceFile` a finding belongs to, so the representation works well as long as every
span in a diagnostic came from one file. The boundary becomes awkward in two places:

- macro-authored tokens may originate in a different module, but definition-site byte
  offsets cannot be carried into the caller's expanded AST because rendering would
  interpret those offsets against the caller's source; generated tokens therefore use
  call-site spans and keep definition-site *resolution* provenance in `Origin` instead;
- a `Diagnostic` cannot safely contain labels from two files, because each label is an
  unqualified byte range and `Renderer` receives exactly one `SourceFile`.

This is internally consistent today, but it makes file identity an ambient convention
instead of part of the type system. A future source-aware location model — for example
`SourceId` plus `Span`, or a compact `SourceSpan`/`Site` wrapper — would make cross-file
locations representable and would let macro diagnostics distinguish call-site and
definition-site locations without overloading one byte-offset space. This is breaking
across the frontend, HIR, driver, analyzer, and diagnostic APIs, so it should be a
deliberate architecture change rather than part of a local refactor.

### Nested statement fields lose their own source site

A few AST nodes discard a more specific source site that later phases could use. Nested
statements in `ForStmt::init` (`Option<Statement>`) and `DeferStmt::body` (`Box<Statement>`) lose
the `StatementNode` wrapper that normally carries the statement span; `ForInStmt` stores its
binding without a dedicated name span; and `SelfMode` stores the parsed self mode without the span
of the `self` token. HIR lowering therefore has to reuse an enclosing `for`/`defer`/function span
for those synthetic inner nodes instead of preserving the exact source site.

This is harmless for execution today, but it weakens diagnostic precision and makes the AST/HIR
boundary less regular. The clean fix is a deliberate AST shape change: nested syntactic statements
should retain `StatementNode` (and bindings should retain their identifier span/site) rather than
having lowering reconstruct location information from the parent. That change propagates through
parser consumers and HIR lowering, so it belongs in a focused frontend API change rather than this
refactor pass.


### `HirFor::init` is plural even though source grammar permits one initializer

The classic `for` AST stores at most one initializer statement, but `HirFor::init` is a
`Vec<HirStmt>`. HIR lowering therefore has to wrap the single lowered statement in a one-element
vector. The shape appears to be historical rather than a current lowering requirement: macro
expansion is complete before HIR, and the classic `for` initializer grammar itself does not lower
one source initializer into multiple HIR statements.

Changing the field to `Option<Box<HirStmt>>` (or another explicit zero-or-one representation) would
make the invariant visible in the type system and remove downstream "could there be many?" mental
overhead. It changes the public HIR shape and every analyzer/MIR consumer, so it should be handled
as a focused cross-crate representation change rather than folded into a local frontend refactor.
