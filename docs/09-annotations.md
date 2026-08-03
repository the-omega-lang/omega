# Annotations

```
@layout(pack = sizeof<usize>, align = sizeof<usize>)
struct CLikePacking { ... }

@inline
@suppress(inline_not_enforced)
fast_fn() => void { ... }

@mangling(disabled)
raw_add(a: i32, b: i32) => i32 { a + b }
```

Grammar: `'@' Ident ['(' Arg (',' Arg)* ')']` above a struct/enum/union/
function declaration. Parens are optional (bare `@inline` and `@inline()`
both mean zero arguments). An argument is a bare identifier or a `key =
value` pair, where `value` is a raw integer literal, `sizeof<Type>` (scoped
to primitive types only when used here — see below), or a string literal
(`@mangling(force = "...")`'s own argument — see below).

Called "annotation," not "attribute" — deliberately: "attribute" reads as
vague, "annotation" is explicit about talking directly to the compiler.
Applying an annotation to an item kind that has no annotation surface at
all (`extern`/`import`/plain declaration/macro/spec) is a hard parse-time
error, not silently dropped.

Resolution happens entirely in `omega-analyzer` (never in HIR lowering,
which stays fully infallible, and never re-derived in codegen) — resolved
once, at signature-collection time, and read back everywhere downstream
rather than re-resolved per use. This "resolve once, at signature time"
placement is load-bearing, not just tidy: see the `@mangling` + `extern`
fix below for what breaks if it isn't followed.

## `@layout(pack = n, align = n)`

Struct/enum fields are packed with **no** alignment by default (`pack = 1,
align = 1` — today's implicit behavior, a true no-op). `@layout` overrides
either or both:

- **`align`** — the whole type's own alignment when embedded elsewhere
  (trailing padding rounds the type's total size up to a multiple of this).
- **`pack`** — C-style chunk-sharing granularity: a chunk of size `pack`
  starts at every multiple of `pack`; a field lands in the current chunk if
  it fits in what remains, or if it would be the *first* thing in that
  chunk (this second condition is what lets a field bigger than `pack`
  itself still get placed, instead of endlessly failing to "fit"); otherwise
  padding advances to the next chunk boundary.

Layout is not just a byte-offset bookkeeping concern in this codegen: a
struct/enum value passed as a register-backed parameter is literally its
flattened list of scalar IR leaves (see
[primitives](01-primitives.md)), so interior/trailing padding has to exist
as *real filler leaves*, not just an offset table entry, or the leaf-list
and memory-byte-offset views of the same value would silently disagree.

Unions do not support `@layout` at all (always alignment 1, matching their
pre-annotation layout exactly).

## `sizeof<Type>`

```
sizeof<CLikePacking>
sizeof<usize>
```

An ordinary expression (type `usize`), fully general for any type codegen
already knows how to size — struct, enum, primitive, anything. Inside an
annotation argument specifically (`@layout(pack = sizeof<usize>)`), it's
deliberately **scoped to primitive types only** and resolved eagerly during
analysis rather than deferred to codegen — resolving an aggregate type's
size there would force either threading fallible target-dependent
resolution through ~25+ codegen call sites, or picking the same hardcoded
convention `numeric_kind`/`cast_class` already use for primitives. The
narrower rule was chosen; `sizeof<SomeStruct>` used inside an annotation
gets an immediate, clear error rather than a deferred one.

## `@inline`

A hint. **Always a no-op today** — there is no Cranelift/`cranelift-module`
per-function inlining hook in this codegen at all — so every `@inline` use
produces a warning (`InlineNotEnforced`), suppressible via
`@suppress(inline_not_enforced)`. This is deliberate, documented behavior
(per the spec: "if unavailable, only a warning"), not a bug — it will stop
warning once real inlining support lands.

## `@mangling(disabled)` / `@mangling(force = "...")`

`disabled` emits the function under its bare, unmangled name
(`Linkage::Import`-only for an extern reference, no `Export` pairing needed
there). Rejected outright on struct/enum/union methods and on any function
with generics — the `$$N`-style instantiation disambiguation is the only
thing preventing distinct instantiations from colliding once mangling is
off entirely, and a bare method name has no owning-type prefix to keep it
from colliding with an unrelated same-named method elsewhere.

`force = "some_symbol_name"` instead gives the function an *exact*,
caller-chosen linker symbol, verbatim — no mangling scheme applied at all,
not even the bare-name fallback `disabled` uses. Unlike `disabled`, `force`
**is** allowed on a struct/enum/union method: the name is a complete,
deliberate choice, so there's no bare-name collision risk to guard against
the way `disabled` has. Still rejected on a generic function, for a
stronger reason than `disabled`'s: every instantiation would share the
exact same hardcoded name, an *unconditional* collision, not merely a
possible one. An empty string is rejected outright (`'force' needs a
non-empty symbol name`).

Either way, a whole-program duplicate-symbol check catches two different
declarations that would collide on the same final symbol (whether from two
`disabled` functions sharing a name, or two `force`d names coinciding) and
turns it into a compile error rather than a `cranelift_module` panic or a
silent linker failure.

There's a third, internal-only mode, `ManglingMode::Glued`, with no
`@mangling(...)` spelling of its own — the compiler applies it, never the
user — that a `@glue` marker's methods get forced into automatically, so
they land on the exact same symbol their `@gap` spec's own synthesized
declaration expects. See [gaps-and-glue.md](21-gaps-and-glue.md).

**The fix that generalized this feature's own placement**: an extern
function's own `@mangling(disabled)` used to be invisible to a consumer's
`--extern` reference, because mangling was originally resolved at
*body*-check time — and an extern function's body is never checked by the
consuming compilation, only its signature is. Fixed by moving **all**
function annotation resolution to signature-collection time uniformly
(which every function this compilation knows about goes through, local or
extern-referenced, body-checked or not) — a structural fix, not a
mangling-specific patch, so the same class of gap can't recur for a future
annotation.

## `@suppress(warning_name, ...)`

Silences the named warning(s) within the annotated item's own scope (and,
for `@inline`, the item's own body specifically). Names are never validated
for existence — warnings can be renamed or removed later, and a bad
`@suppress` name should never itself become a new error. Every warning's
own diagnostic footer shows its `@suppress`-matching name, mirroring
rustc's `` `#[warn(...)]` on by default `` convention.

## Caveats

- `@inline` has no enforcement mechanism at all yet — purely a hint plus a
  warning, until real inlining support exists.
- `sizeof<Type>` inside an annotation argument is primitive-only; the
  ordinary expression form has no such restriction.
- A fifth annotation, `@ufcs`, was explicitly designed and then not
  pursued this way at all — see [specs](08-specs.md)'s `for`-attachment
  mechanism, which replaced the idea entirely rather than shipping
  alongside it.
