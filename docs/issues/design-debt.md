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

### Deferred question: restricting variant construction to the declaring module

`Option` and `Result` have static constructors (`Option::some(v)`,
`Result::err(e)`), but variant literals such as `Option<T>::Some { value = v; }`
stay legal everywhere. Making the constructors the only way to build these
types would need a general rule, not an `Option`/`Result` special case: a way
for an enum to declare its variants constructible only inside its declaring
module, as a struct's private fields already restrict its literal. No such rule
exists yet, so nothing is restricted. Any proposal must settle how it
interacts with pattern matching (which names variants without constructing
them), with `?` (which the compiler lowers to variant construction), and with
generic code outside the declaring module.

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
