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

## Confirmed soundness bugs

These compile and run today; both were verified against the real compiler, not
just read from source. Both are also listed in 
[known issues](14-known-issues.md).

### Enum tag/header write-protection only guards plain `=`

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
`immutable_enum_member`/`EnumFieldImmutable` (`omega-analyzer/src/
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
codebase's known failure mode (see the `hidden` finding right below, and the
README's own "resolve once, read back everywhere" pillar) — a constraint
enforced at exactly one of several structurally-equivalent call sites instead
of at the single choke point (`analyze_place` itself, or `require_mutable_place`
) every write-position already funnels through.

### `\&hidden` silently drops the bypass through a slice/array-literal position

```
struct Box { data: [i32; 4]; }

peek_whole(b: *Box) => *[i32; 4] { &hidden b.data }         # works
peek(b: *Box) => *[i32] { &hidden b.data[0...1] }         # fails:
# error: 'data' on 'Box' is not visible here
#   = help: mark the field `exposed`/`internal` on `Box`, or bypass with `hidden`
```
Verified: the `peek` version fails with a visibility error whose own suggested
fix ("bypass with `hidden`") is already present in the source that just failed. `
07-visibility.md` already documents one shipped bug in this exact family (`
hidden abc.number = 10;` losing its bypass because `=` is handled one level
above where `Hidden` sits) and describes the fix as "explicitly re-checking for
a stripped `Hidden` wrapper... at every genuine target/operand position that
isn't itself call/postfix syntax." This is a second, unfixed occurrence of the
identical class: `HirExpr::AddressOf` (`analysis.rs:4852-4878`) computes `
was_hidden := Self::strip_hidden(base)` but only threads it into `
with_hidden_bypass` on the final bare-`Place` branch — the `Slice` and `
ArrayLiteral` early-return branches (`analyze_slice`/`analyze_const_slice`)
never see it, so no bypass frame is pushed at all. Because no frame is pushed, `
UnnecessaryHidden` can't fire either — the failure has no diagnostic trail
pointing at the real cause; it just looks like `hidden` silently didn't work.

Worth treating as a pattern, not a one-off patch target: `hidden`'s correctness
depends on every current and future write/borrow position individually
remembering to re-check for a stripped wrapper. That's a "remember to do this
everywhere" invariant with no compiler-enforced backstop — precisely the shape
of bug this project's own commit history shows it already hit twice.

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

`total_bytes`/the packed-layout doc comment (`omega-codegen/src/lib.rs:397-405`)
justifies "packed by default, no implicit alignment" as safe with: "x86_64
tolerates unaligned loads/stores with no correctness issue, so packed is safe
as a default." That's a real, correct fact about x86_64. But `
omega-codegen/src/target.rs` already defines `Arch::Aarch64` as a genuine,
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
omega-codegen/src/lib.rs:352`) — the same relationship `\*[u8]` has to `\*str`
(different nominal types, identical runtime shape, deliberately no implicit
coercion, but at least an explicit cast exists both ways). `Array` gets neither:
it's absent from both `cast_class` and `accepts` (`resolved_type.rs`), so
there's no way — implicit or explicit — to convert between `\[T]` and `\*T` even
though they're bit-identical at runtme and one is very obviously "the C way to
spell" the other. This is a narrower, lower-stakes version of the
fat-pointer-family pattern the rest of the language already handles
consistently; it just wasn't extended to this one older, legacy shape.

## Minor rough edges

### `hidden hidden x` is accepted by the parser and always produces a spurious `UnnecessaryHidden` warning

`parse_unary` recurses freely on its own prefix set with no special-casing for a
doubled `hidden`, so `hidden hidden x` lowers to a nested `Hidden(Hidden(x))`
with two stacked bypass frames. `check_visibility`/ `check_member_visibility`
only ever mark the *innermost* active frame as used, so if the bypass is
genuinely needed and gets consumed while evaluating `x`, the *outer* `hidden`'s
frame is never marked used and always reports `UnnecessaryHidden` — even when
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

## Summary table


|Finding|Kind|Verified how|
|-|-|-|
|Enum tag write bypass via `\+=`/`\&mut`|soundness bug|real compile|
|`\&hidden base\[range]` drops the bypass|soundness bug|real compile|
|`(a >= x) & (a <= y)` doesn't compile|doc/code contradiction|real compile|
|"Nominal, not structural" is false for all-required specs|doc/code contradiction|source read, cross-referenced against 07-visibility.md's own contradicting line|
|`bool` has zero operators, including `!`|design asymmetry|source read|
|variant-match vs. tag-match exhaustiveness split|design asymmetry|source read|
|conflicting spec defaults resolve silently by order|design gap|source read|
|packed layout's safety argument is x86_64-only, `\--target` offers aarch64|latent assumption|source read|
|`cast_class` hardcodes pointer width to 64|latent assumption|source read|
|`Array` has no coercion or cast to/from `Pointer`|narrow gap|source read|
|`hidden hidden x` always warns spuriously|minor|source read|
|implicit enum tag has no width bound-check|minor, theoretical|source read|

