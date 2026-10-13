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
