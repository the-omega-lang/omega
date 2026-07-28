# Design review: unsoundness, inconsistencies, and rough edges

This file is a different kind of document from 
[known issues](14-known-issues.md). That file tracks *confirmed, scoped gaps* —
"X doesn't work yet." This one tracks things that **do** work, as implemented,
but don't make sense on reflection: asymmetries with no principled reason,
places where the implementation quietly contradicts what the rest of the
documentation claims about it, and syntax/design choices that are load-bearing
today but deserve a second look. Some entries here are genuine bugs (verified
by compiling real repros, not just read from source) promoted to 
[known issues](14-known-issues.md) as well; most are not bugs at all, just weak
spots worth deeper thought before they calcify further.

Nothing in this file was fixed as part of writing it — this is a mapping
exercise, not a fix pass.

## Fixed: two confirmed soundness bugs

**Both of these are now fixed**, during the `omega-analyzer` restructure that
split its 7 500-line `analysis.rs` into eleven per-concern modules. The
writeups are kept because the *shape* of each is what matters, not the
individual repro; both were verified fixed against the same repros that first
demonstrated them, and every object file this project builds is byte-identical
across the restructure itself.

### Fixed: enum tag/header write-protection only guarded plain `=`

```
enum MyEnum(tag: u16) { A(1), B(2) }

main() => i32 {
    mut e := MyEnum::A;
    e.tag = 5;        # correctly rejected: "cannot assign to 'tag' of an enum value"
    e.tag += 1;        # compiles clean — same field, same write, no error
    p := &mut e.tag;
    *p = 99;             # compiles clean too
    0
}
```
`immutable_enum_member`/`EnumFieldImmutable` (`compiler/omega-analyzer/src/
analysis.rs:1881-1887`) is the check that makes plain assignment to a `.tag` or
header field a hard error — correctly, since these are supposed to be
per-variant compile-time constants, not writable storage. But it's called from
exactly one place: `HirExpr::Assignment`'s arm (`analysis.rs:4822-4829`). `
analyze_compound_assign` (`analysis.rs:4465-4504`, backing `\+=`/`\-=`/etc.) and `
HirExpr::AddressOf`'s `\&mut` arm both build the identical `CheckedPlace`
through `analyze_place`, then only call `require_mutable_place` — which checks
that the *root binding* is `mut`, nothing about whether the specific field is a
constant tag/header slot. Once you have a live `\*mut u16` pointing at a tag,
you can write through it and desynchronize the enum's tag from its actual
variant layout with zero casts and zero `unsafe`\-equivalent — every downstream `
match` on that value is now trusting a lie, and per-variant body-field
offsets/interpretation baked in by codegen no longer agree with what the tag
claims.

This isn't a narrow oversight in one call site; it's the general shape of this
codebase's known failure mode (see the `reveal` finding right below, and the
README's own "resolve once, read back everywhere" pillar) — a constraint
enforced at exactly one of several structurally-equivalent call sites instead
of at the single choke point (`analyze_place` itself, or `require_mutable_place`
) every write-position already funnels through.

**Fix**: exactly that — the check moved into `require_mutable_place`, which
every real write already funnels through (`=`, compound assignment, `++`/`--`,
`&mut`, and a `mut self` method call's auto-ref). All five now reject a tag or
header write; the `=`\-only check that used to sit in the assignment arm is
gone, so there is no longer a second place that could drift. Reads, and writes
to a variant's own body fields or the shared dynamic fields, are unaffected.

### Fixed: `\&reveal` silently dropped the bypass through a slice/array-literal position

```
struct Box { data: [i32; 4]; }

peek_whole(b: *Box) => *[i32; 4] { &reveal b.data }         # works
peek(b: *Box) => *[i32] { &reveal b.data[0...1] }         # fails:
# error: 'data' on 'Box' is not visible here
#   = help: mark the field `exposed`/`internal` on `Box`, or bypass with `reveal`
```
Verified: the `peek` version fails with a visibility error whose own suggested
fix ("bypass with `reveal`") is already present in the source that just failed. `
07-visibility.md` already documents one shipped bug in this exact family (`
reveal abc.number = 10;` losing its bypass because `=` is handled one level
above where `Reveal` sits) and describes the fix as "explicitly re-checking for
a stripped `Reveal` wrapper... at every genuine target/operand position that
isn't itself call/postfix syntax." This is a second, unfixed occurrence of the
identical class: `HirExpr::AddressOf` (`analysis.rs:4852-4878`) computes `
was_reveal := Self::strip_reveal(base)` but only threads it into `
with_reveal_bypass` on the final bare-`Place` branch — the `Slice` and `
ArrayLiteral` early-return branches (`analyze_slice`/`analyze_const_slice`)
never see it, so no bypass frame is pushed at all. Because no frame is pushed, `
UnnecessaryReveal` can't fire either — the failure has no diagnostic trail
pointing at the real cause; it just looks like `reveal` silently didn't work.

Worth treating as a pattern, not a one-off patch target: `reveal`'s correctness
depends on every current and future write/borrow position individually
remembering to re-check for a stripped wrapper. That's a "remember to do this
everywhere" invariant with no compiler-enforced backstop — precisely the shape
of bug this project's own commit history shows it already hit twice.

**Fix**: both early-return branches now run under `with_reveal_bypass`, so all
three `&`\-operand shapes (plain place, slice, compile-time slice) activate the
bypass identically. The underlying "every position must remember" invariant is
unchanged and still has no backstop — see the note on it in the compiler
architecture section below.

## Contradictions between documented intent and actual behavior

### The docs' own "combine two comparisons" example doesn't compile

`03-control-flow.md` states "`bool` supports **none** of `== != & | ^`" and
separately, a few paragraphs later in the same file, gives `(a >= x) & (a <= y)`
as the correct way to combine two comparisons since `&`/`|` bind tighter than
comparison. Verified: this does not compile.

```
error: cannot apply '&' to a value of type 'bool'
|^^^^^^^^^^^^^^^^^ `&` requires numeric operands, but this is `bool`
```
`(a >= x) & (a <= y)` produces two `bool`s, and `bool` is excluded from `
numeric_kind` (`resolved_type.rs:640-657` has no `Bool` arm), which is exactly
the gate `analyze_binary_op` uses for every non-comparison operand (`
analysis.rs:4322-4333`). So the docs contradict themselves within one file, and
— more importantly — the actual gap this exposes is real: **there is no way to
combine two `bool`\-valued expressions in this language at all** except nested `
if`, per the very next section:

```
if x { false } else { true }          # NOT x
if x { y } else { false }              # x AND y
if x { true } else { y }                # x OR y
```
This is a documented, deliberate choice (`bool` isn't numeric, so bitwise ops
don't apply, and there's no dedicated `&&`/`||`/`!`) — but the doc's own broken
example suggests even the person who wrote it reached for `&`/`|` as the natural
way to combine two comparisons and didn't actually test it. Combining two or
more conditions today means either deeply nested `if`\-expressions (unreadable
past two or three terms) or restructuring the whole expression around
match/early-return. Worth reconsidering independent of anything else in this
file — this is the single most-reached-for capability ("is this true and that
true") in ordinary code, and today's only answer scales badly.

### "Nominal, not structural" bound-checking is actually structural whenever a spec has no default methods

`06-generics.md` and `08-specs.md` both say, near-verbatim, "Nominal, not
structural — `T: Animal` requires an explicit `struct S : Animal` declaration." `
07-visibility.md:126`, written about a different bug in a different section,
already says the opposite in passing: "a `T: Animal` bound is a **structural fact**
about `T`." Both can't be the stated design; tracing the actual check shows the
second one is what the code does.

`check_generic_bound` → `type_implements_spec` (`analysis.rs:1328-1359`) never
looks at the concrete type's own `: Spec1, Spec2` declaration list. It walks the
spec's *flattened requirement list* and, for each required signature, calls `
find_methods` on the concrete type and compares `fn_type`. `find_methods` scans `
ResolvedStructType::functions` — a flat `Vec<(Ident, ResolvedMethod)>` where
hand-written methods and spec-synthesized defaults are merged and
indistinguishable. So:

```
exposed spec Animal { kind(*self) => i32; }        # single required fn, no default

struct Dog { exposed kind(*self) => i32 { 1 } }    # no ": Animal" anywhere

process<T: Animal>(v: T) => i32 { v.kind() }
process(Dog{});                                    # accepted — structurally, not nominally
```
The "nominal" framing only becomes true in practice once a spec has *at least
one default method* — those only ever land on a type's method list via `
resolve_implements_clause`'s actual processing of a real `: Spec` declaration,
so a spec that's all-required-no-defaults is checked in a way that's
indistinguishable from an unbound generic's own duck-typing. The same `
type_implements_spec` path also backs `spec \*T` dynamic-dispatch coercion, so
this isn't just a generic-bound curiosity — it's the same gap wherever a spec
is used as a constraint rather than a `: Spec`\-declared interface. Not urgent
(nothing is unsound here — an accidental structural match is still a real,
complete implementation), but the documentation's claim is simply false for a
common spec shape, and worth either fixing the check to actually consult the
declaration list, or fixing the docs to admit the real rule ("nominal once a
spec has a default; structural otherwise").

## Design inconsistencies worth a second look

### `bool` is the one primitive with zero operators, including negation

Every other primitive — even `char`, which is explicitly barred from
arithmetic/bitwise/cast for a real, documented soundness reason (an invalid
Unicode scalar value could result) — gets its natural comparison operators. `bool`
, the type that exists purely to be compared and combined, gets none: no `==`,
no `!=`, no `&`/`|`/`^`, and there is no `!` token in the grammar at all (`!`
only appears as part of `!=` or macro-invocation `name!(...)`). The asymmetry: `
char` (arguably the type with the most legitimate reason to restrict operators)
has full comparison; `bool` (the type with the least reason) has none. The
stated rationale — bitwise operators require `numeric_kind`, and `bool` isn't
numeric — explains *why* the current implementation behaves this way, but
doesn't really explain why `bool` couldn't have its own narrow operator set the
way `char` does (comparison without arithmetic). Combined with the previous
finding (the docs' own broken example), this reads less like a settled design
decision and more like a gap nobody has revisited since `mut`/pointers were
bolted on late in the language's history.

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

### Silent order-dependent resolution when two specs provide conflicting defaults for the same signature

`flatten_spec_into`'s dedup rule (`analysis.rs:1096-1178`) compares candidates
by `fn_type` only — params/return/self_mode, never the body. When two *different*
specs each supply a default body for the same name+ signature, the code takes
the `existing.default_body.is_some() && new.default_body.is_some()` branch and
just `continue`s, keeping whichever was flattened first:

```
exposed spec Left  { greet(*self) => i32 { 1 } }
exposed spec Right { greet(*self) => i32 { 2 } }

struct Both : Left, Right {}
# Both{}.greet() silently returns 1 (Left's default) -- no ConflictingSpecFunctions,
# no ambiguity error, no diagnostic of any kind
```
`ConflictingSpecFunctions` only fires on an actual type mismatch, never on a
body mismatch at identical signature — which means the "same name + identical
signature → assume it's the same function" comment justifying the dedup (`
analysis.rs:1076`) is stated as an assumption but never actually verified; the
compiler structurally can't tell whether it's true. This is the one entry in
this file that plausibly *should* just become a `ConflictingSpecFunctions` check
(compare default bodies structurally, or require an explicit override whenever
two dependency defaults collide) — unlike the other entries, there's no real
design tension pulling the other way.

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
note on `isize`/`usize` in `01-primitives.md`: `cast_class` and `numeric_kind` (`
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

### `ResolvedType::Array` (decayed thin-pointer array) has neither implicit coercion nor an explicit cast to/from `Pointer(T)`

`\[T]` (a bare, unsized decayed-array parameter shape like `argv: \[*u8]`) is
runtime-identical to a single thin pointer — one leaf, no length (`
compiler/omega-codegen/src/lib.rs:352`) — the same relationship `\*[u8]` has to `\*str`
(different nominal types, identical runtime shape, deliberately no implicit
coercion, but at least an explicit cast exists both ways). `Array` gets neither:
it's absent from both `cast_class` and `accepts` (`resolved_type.rs`), so
there's no way — implicit or explicit — to convert between `\[T]` and `\*T` even
though they're bit-identical at runtme and one is very obviously "the C way to
spell" the other. This is a narrower, lower-stakes version of the
fat-pointer-family pattern the rest of the language already handles
consistently; it just wasn't extended to this one older, legacy shape.

## Minor rough edges

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

## Compiler architecture

Added while restructuring `omega-driver` and `omega-analyzer` (see
[modules-and-linkage.md](10-modules-and-linkage.md)'s own note on those
passes). Everything below **works today** — these are shape problems, and each
one is a breaking change to a cross-crate interface or a core key/identity
type, which is why none of them were done as part of a refactor.

### `omega-driver`

#### Overloading is a second, parallel item pipeline that exists only because the query key can't name a candidate

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

The fix is to make the key able to name a candidate (a disambiguator
alongside `name`, its declaration position, `0` for the overwhelmingly common
unambiguous case), after which both parallel caches, both extra sweeps, and
at least one trait method collapse into the ordinary path — and generic
overloads become possible rather than structurally excluded. Breaking:
changes the resolver trait's surface and every cache key shape.

#### Two independent "pending body" queues that differ only in whether the owner has a declared item

A spec default method with no override is checked *after* its implementor's
signature, in a second pass. There are two entirely separate machines for
this: one keyed by the implementor's `ItemKey`, drained inside that
implementor's own body check; and one keyed by the receiver's `ResolvedType`,
drained by its own `while let` loop after both phases finish (because
checking one queued body can queue another for a different receiver).

Both queue the same `PendingSpecMethod`, both check it with a fresh analyzer
seeded with the same shape of substitution, and both merge the result into a
module's item list. The only real difference is that a `for`-attached
receiver (`i32`, `[T]`) has no declared item to key on, so it also has to
carry its own module path. One queue keyed by an owner enum
(`Item(ItemKey) | Receiver(ResolvedType)`) would delete the duplicate
machine; the `while let` drain shape is the correct one for both.

#### `core` is hardcoded as the one and only extension root

`for`-attached methods are found by walking the import graph of the module
literally named `core`, and declaring a `for` block anywhere else is a hard
error (`ExtensionOutsideCore`). Consequences worth deciding on deliberately
rather than inheriting:

- no third-party package can ship extension methods, ever;
- a local module legitimately named `core` (or any package built with
  `--name=core`) silently *becomes* the extension root;
- the "at most one `for` block per target type" rule is enforced across one
  tree, so it would not survive a second root without being re-thought.

A declared capability (`--extension-root=<name>`, repeatable, or a per-package
manifest bit) would replace the literal, with the one-block-per-target rule
enforced across all registered roots at once.

#### `ResolveError::Cycle` carries a chain it never populates

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

#### Module paths and item paths are the same untyped `Vec<Ident>`

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

#### Diagnostic scoping for borrowed modules is three ad-hoc lists

Which findings surface depends on which of three lists a module lands in, with
four different outcomes: errors from a local module surface; errors from an
extern module are dropped; errors from `core`'s tree surface (it's added to
the error scope explicitly, so a broken `for` block still reports); warnings
from `core` are dropped (deliberately — its unused imports shouldn't leak into
every downstream build); warnings from other externs never exist because their
bodies are never checked.

Each individual choice is defensible and documented at its site, but there is
no single stated policy, so the next kind of borrowed module has no rule to
follow. The underlying distinction is ownership: a module is either *compiled*
by this invocation or merely *scanned* for it, and a scanned module should
report exactly the findings that are about something this invocation asked for.
Stating that once, and deriving the scopes from it, replaces all three lists.

### `omega-analyzer`

#### A node's identity is two parameters everywhere, threaded by hand

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

#### `match` value arms must *partition* the domain — there is no catch-all

Arms may not overlap at all (by design: no first-match-wins), and this
interacts badly with the natural way to write a total match:

```
match ch {
    'a'...'z' => 1,
    '0'...'9' => 2,
    ...       => 3,        # rejected: overlaps both arms above
}
```

The only legal totals are exact partitions (`0..<100`, `100`, `101...`), so a
"everything else" arm can never be written, and adding a new specific arm to
an existing match means editing a neighbouring arm's bounds to make room. An
`else` block covers the same need for a *non*-exhaustive match, so the gap is
narrow -- but the asymmetry (`else` exists, `...` doesn't) is worth a
decision rather than an accident. The diagnostic now explains the rule
explicitly (see the fix note below); the rule itself is unchanged.

#### `reveal` still has no backstop for the "every position must remember" invariant

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

## Summary table


|Finding|Kind|Verified how|
|-|-|-|
|Enum tag write bypass via `\+=`/`\&mut`|soundness bug (**fixed**)|real compile, before and after|
|`\&reveal base\[range]` drops the bypass|soundness bug (**fixed**)|real compile, before and after|
|`(a >= x) & (a <= y)` doesn't compile|doc/code contradiction|real compile|
|"Nominal, not structural" is false for all-required specs|doc/code contradiction|source read, cross-referenced against 07-visibility.md's own contradicting line|
|`bool` has zero operators, including `!`|design asymmetry|source read|
|variant-match vs. tag-match exhaustiveness split|design asymmetry|source read|
|conflicting spec defaults resolve silently by order|design gap|source read|
|packed layout's safety argument is x86_64-only, `\--target` offers aarch64|latent assumption|source read|
|`cast_class` hardcodes pointer width to 64|latent assumption|source read|
|`Array` has no coercion or cast to/from `Pointer`|narrow gap|source read|
|`reveal reveal x` always warns spuriously|minor|source read|
|implicit enum tag has no width bound-check|minor, theoretical|source read|
|overloading needs a whole parallel item pipeline|architecture|source read, whole-crate restructure|
|two separate pending-spec-method queues|architecture|source read, whole-crate restructure|
|`core` hardcoded as the only extension root|design gap|source read|
|`ResolveError::Cycle` never populates its chain|diagnostic gap|source read, both construction sites|
|module paths and item paths share one untyped shape|architecture|source read|
|borrowed-module diagnostic scoping has no stated policy|design gap|source read|
|`(HirId, Span)` threaded as two parameters everywhere|architecture|source read, whole-crate restructure|
|value-`match` arms must partition the domain, no catch-all|design gap|real compile|
|`reveal`'s "every position must remember" invariant has no backstop|latent bug class|real compile (third occurrence fixed)|

