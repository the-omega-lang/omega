# Design debt

Unresolved design/architecture inconsistencies migrated from the former monolithic design-review document. Resolved review findings are intentionally omitted.

### Deferred question: disabling compiler-generated invariant panics

**Current direction:** retain the checks. No disabling flag or replacement
semantics are approved. Removing defined failure behavior for a potentially
minor optimization may be a poor tradeoff; revisit only with a concrete need
and evidence of the cost.

The compiler currently panics on invalid enum tags in `match`, anonymous-enum
widening, and `?`, and when a call declared `never` returns. These checks share
the machinery described in [Runtime checks](../architecture/mir-and-codegen.md#runtime-checks).
An enum match is exhaustive over declared variants; `else` and bare `..` cover
remaining valid variants, while a separate implicit failure path catches
invalid tags. See [Invalid tags](../language/enums-and-pattern-matching.md#invalid-tags).

The unresolved question is what replaces that failure path when automatic
panics are disabled:

- **Trust the invariant:** invalid states become undefined behavior. Exhaustive
  matching still covers valid values, but there is no guaranteed outcome for
  an invalid tag. Replacing the panic with `unreachable` is an optimizer
  assumption, not a guaranteed stop.
- **Require explicit handling:** one proposal is to require an `else` on every
  enum match in this mode and let it catch invalid tags. This retains coverage
  of invalid states, but an `else` can then contain both unmatched valid variants
  and invalid representations. It cannot justify the same variant/payload
  refinement as today's valid-variant-only fallback. It also makes source
  acceptance and the meaning of `else` depend on the compiler mode; the
  relationship with bare `..` would need a decision.
- **Separate invalid-state handling:** preserve ordinary `else` and provide an
  explicit handler for broken invariants, with the compiler-supplied panic as
  the default. Syntax, scope, and obligations remain undecided.
- **Retain termination with a different mechanism:** a target-supported trap
  could avoid the panic-handler call while preserving a defined stop, but
  would still require enforcement and a platform contract.

A match-only solution is insufficient: `?`, widening, and unexpected returns
from `never` calls need a coherent policy too. Handling an invalid tag must
not project a variant payload without proof, and recovery at a match does not
by itself make invalid enum values safe to hold, copy, or pass around. Any
future proposal should distinguish compiler-generated checks from explicit
source-level panic calls and weigh actual optimization gains against these
semantic costs.

### Enum match dispatch re-reads the tag for each condition

Named and anonymous enum matches currently carry a separate tag-place read in
each arm condition, including the conditions for `..` and a reachable `else`.
This does not repeat evaluation of the scrutinee expression, but dispatch can
load its tag repeatedly. The conditions are built by `tag_variant_condition`
and `member_tag_condition` in
[`analysis/patterns.rs`](../../compiler/omega-analyzer/src/analysis/patterns.rs)
and lowered independently by
[`lower_match_chain`](../../compiler/omega-mir/src/lower/function/control_flow.rs).

This is an optimization follow-up, not a known failure of the invalid-tag panic
check: these are ordinary field loads with no intervening user code during
dispatch. Redundant loads may survive without optimization; the current
representation leaves their elimination to the backend.

Follow up by sampling the tag once before dispatch and sharing that value
across all conditions. Preserve binding refinement, legal-variant-only `else`,
and the separate invalid-tag panic path. Verify one tag read in MIR for named
and anonymous enums (including pointer scrutinees and sparse explicit tags),
and retain executable coverage of valid arms and invalid-tag panics at `-O0`
and `-O3`.

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

### Packed-by-default layout has no per-target safety argument

Struct and enum layout defaults to `pack = 1, align = 1`, and `type_alignment`
(`compiler/omega-analyzer/src/layout.rs`) reports `1` for every type that neither
declares `@layout(align)` nor contains something that does — primitives
included. The escape hatch itself is sound: an explicit alignment now propagates
outward through inline containment and is a real address guarantee. What is
unjustified is the *default*. The justification it was originally written under
("x86_64 tolerates
unaligned loads/stores with no correctness issue, so packed is safe as a
default") is no longer in the source, but nothing has replaced it, and `Arch`
now names eight architectures rather than the one that argument was true of.
AArch64 in particular gives no comparable blanket guarantee: several
OS/embedded configurations enable strict alignment-fault checking globally, and
SIMD load/store forms require natural alignment — forms this backend does not
emit today but would.

Atomics are no longer an instance of this: they require natural alignment by
contract now ([`atomics.md`](../language/atomics.md)), settled per-feature
rather than by the layout default. What stays open is the default itself —
whether "packed unless annotated" is the right whole-language choice on targets
where unaligned ordinary access is not free or not permitted — and that nothing
re-derives or gates it per-target.

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
