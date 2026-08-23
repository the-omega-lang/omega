# Design debt

Unresolved design/architecture inconsistencies migrated from the former monolithic design-review document. Resolved review findings are intentionally omitted.

### Enum-variant matching and enum-tag matching are two unrelated exhaustiveness engines with very different practical requirements

The docs describe one unified exhaustiveness mechanism (`exhaustiveness.rs`)
covering "enums, integers, `bool`, and `char`." In truth `analyze_enum_match`
(matching the enum value itself, `analysis/patterns.rs`) never touches
`exhaustiveness.rs` — it just tracks a `HashMap<usize, Span>` of covered variant
indices and requires exactly one arm per variant, however many variants there
are. `analyze_value_match` (matching an integer/bool/char scrutinee in the same
module) is the one that actually runs the interval-sweep
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
- its own `ModuleResolver` method (`resolve_overload_set`), and a lazy
  `ImportTarget::ItemPath` binding that exists *only* so an import of an
  overloaded name isn't eagerly collapsed to one winner;
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

A static-spec parameter (`f(x: spec A + B)`) manifests the identical
"candidate forced non-generic" limitation: `normalize_static_spec_params`
(`omega-analyzer/src/generics.rs`) synthesizes an anonymous bounded generic
for the parameter, so an *overloaded* function that also takes a static-spec
parameter hits this same parallel-pipeline gap (`ensure_overload_signature`
analyzes each candidate's signature with an empty substitution list, which a
static-spec-turned-generic candidate cannot resolve under). This is the same
root cause as the rest of this entry, not a second one — fixed by the same
`ItemKey` disambiguator work, not by a local workaround in alias or
static-spec code.

### A static-spec parameter's synthesized generic uses a fabricated impossible-source name instead of a real identity

`normalize_static_spec_params` (`omega-analyzer/src/generics.rs`) rewrites
`f(x: spec A + B)` into an ordinary anonymous bounded generic parameter so
the rest of the compiler can treat a static-spec parameter exactly like any
other generic. The synthesized parameter's identity is
`Ident(format!("$Param{index}"))` — a string that can never collide with a
real source identifier (`$` is not legal in Omega identifier syntax), used
purely as a collision-safe internal name rather than a semantic
generic-parameter identity with its own origin/provenance metadata.

This is deliberate, collision-safe compatibility representation, not a
correctness bug: two different static-spec parameters in the same function
get distinct `$Param0`/`$Param1` names, and nothing currently depends on the
name meaning more than "this slot." The debt is that a real generic
parameter has an origin-tracked identity a diagnostic or downstream query can
point at meaningfully, while `$ParamN` is source-position-shaped text with no
backing declaration — a diagnostic that needs to name *this* parameter
specifically (rather than pointing at the parameter's own span, which still
works fine today) has nothing better to say than the fabricated name.

The fix is a real `GenericParamId`/anonymous generic-node representation with
its own origin metadata, replacing the current `Ident`-keyed fabrication
everywhere a generic parameter's identity is threaded (substitution maps,
bound-checking, alias-application obligations). Breaking: touches every
generic-parameter-keyed data structure across the analyzer and driver, so it
is its own dedicated task rather than a local patch.

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


### `CodegenRequest::entry` outlives the phase that owns entry-point identity

`omega-mir::lower_program` consumes the checked program's entry path when it
constructs MIR, but the public `omega_codegen::CodegenRequest` still carries the
same path into native emission even though codegen never reads it. Removing the
field would make the phase boundary more honest and shrink the request to facts
codegen actually consumes, but it is a public struct-shape change for callers that
construct requests directly. Remove it in a deliberate API-breaking cleanup rather
than silently as part of a refactor.

### `Driver::compile(&mut self)` presents a reusable object even though compilation state is one-shot

`Driver` owns mutable module/index caches, item/spec query results, import usage,
primitive/conformance registrations, diagnostics, synthetic-ID allocation, and
materialized generic bodies. `compile` appends/populates those structures and does
not reset them before another invocation. The current CLI constructs one `Driver`
and compiles exactly once, so that lifetime is internally coherent, but the public
method takes `&mut self` and therefore advertises a repeatable operation it cannot
soundly promise. Reusing the same driver can retain failed queries, duplicate
registrations/materialized bodies, or mix diagnostics from the previous run.

Target ownership exposes the same ambiguity: `Target` is passed to `Driver::new`
and again to `compile`, where the latter overwrites the stored target.

The long-term API should make the lifetime explicit. The simplest current shape is
to make compilation consume the driver. A more extensible shape is a long-lived
workspace/module store that creates a fresh one-shot `CompilationSession` containing
target-specific semantic/query state. That second design is also the cleaner path
toward incremental compilation. This is a public driver/API change and should be
done deliberately rather than hidden in a refactor.

### Nominal semantic identity is coupled to pervasive `Rc<RefCell<Resolved*Type>>` cells

Structs, enums, unions, and specs establish recursive identity by allocating a
shared `Rc<RefCell<...>>` cell before all semantic facts are known and filling that
cell during signature analysis. This solves declaration-order/recursive-reference
requirements today, but it makes interior mutability part of the semantic type
model consumed across analyzer, driver, MIR, layout, and codegen. Correctness then
depends on phase conventions such as "this cell is complete before this consumer
borrows it," with violations expressed as runtime borrow failures rather than type-
or query-level states.

Before Omega pursues parallel or incremental semantic analysis, consider moving
nominal identity to stable interned IDs backed by an arena/query store. Recursive
types can refer to an ID immediately; resolved fields/layout/method facts can be
queried through explicit completion states instead of mutating a cell embedded in
every `ResolvedType`. This is a deep cross-crate representation change, so the
existing cells should remain until that architecture is designed as a focused
project.

### `reveal` still has no backstop for the "every position must remember" invariant

Reveal activation is now centralized substantially more than it used to be:
`RevealState` owns nested frame bookkeeping and common operand positions go
through `with_reveal_operand` / `with_reveal_bypass`. A hidden/shared access
marks every active frame used, so a nested reveal chain no longer produces the
old guaranteed false warning.

The remaining weakness is architectural: the place resolver itself does not
own reveal activation. Call/assignment/address-of paths still have to enter the
shared helper before they ask place resolution to inspect a revealed operand.
A new syntactic position can therefore bypass the helper and silently lose the
visibility bypass. The structural end state is to make a revealed operand/place
an explicit input shape (or have place resolution activate it itself), so this
cannot be forgotten by a new caller. That change cuts across analysis entry
points and should be designed deliberately rather than hidden in a local
refactor.

### `ModuleResolver` is a broad semantic service facade rather than one coherent capability

`omega-analyzer::resolver::ModuleResolver` is the correct dependency direction — the analyzer does not own filesystem/module/query state — but the trait has accumulated too many unrelated capabilities. One implementation currently provides macro-origin metadata, import and module navigation, item visibility and lookup, generic declaration shapes, overload groups, synthetic IDs, spec declarations, primitive methods, conformance proving/enumeration, checked function bodies for compile-time execution, and resolved `comp` values.

That breadth couples almost every analyzer concern to the entire driver facade, makes focused analyzer tests/mocks expensive, and means adding a new semantic service often grows the same central trait even when the consumer only needs one capability. Splitting it mechanically now would mostly create trait plumbing, so this refactor keeps the facade intact.

Before incremental/parallel compilation or a more independently testable analyzer becomes a priority, design a narrower query boundary: either capability traits (name/module lookup, generic signatures, conformance queries, compile-time body/value queries, synthetic identity) composed by the analyzer, or an explicit semantic query database with focused handles. The goal is not “more traits”; it is that each analysis subsystem depends only on the facts it can actually request. This is a cross-crate architectural/API change and should be reviewed as such.

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
