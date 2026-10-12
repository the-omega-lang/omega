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
[`analysis/patterns/resolve.rs`](../../compiler/omega-analyzer/src/analysis/patterns/resolve.rs)
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

### Matching on an enum's `.tag` is checked against the tag type's whole domain

Every `match` is checked by one pattern-space engine (`exhaustiveness.rs`), but
the domain it checks against comes from the scrutinee's type. Matching the enum
value itself covers its declared variants; matching its `.tag` field is an
ordinary integer match over the tag type's full range:

```
enum E(tag: u32) { First(10), Second(20) }

match e { E::First => .., E::Second => .. }             # exhaustive, no `else` needed

match e.tag { 10 => .., 20 => .. }                        # `else` REQUIRED --
                                                             # the u32 domain has
                                                             # no idea only 10/20
                                                             # ever occur
```
Both behaviors are individually correct for what each match knows, but the
moment you reach for `.tag` instead of the value itself (plausible whenever you
want the numeric tag for logging/serialization/FFI alongside a match),
exhaustiveness checking gets categorically weaker with no warning that anything
changed.

### Annotation holes are synthesized parameters with fabricated names

A binding annotation's `_` holes are solved by rewriting each one to a
synthesized `$Hole{n}` generic parameter (`analysis/items/holes.rs`), building
the annotation's pattern by looking those names up, and re-resolving the
annotation under a substitution that binds them. Inference itself is
positional, but the holes still reach their positions through names no source
can write, the same shape the old `$ParamN` static-spec parameters had.

The structural fix is to build the annotation pattern directly from the
written syntax, with each hole a position of its own, and to materialize the
solved annotation from that pattern rather than from rewritten syntax.

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
