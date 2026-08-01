# Zero-sized types (`marker`)

```
spec GlobalAllocator {
    alloc(*mut self, size: usize) => *u8;
    free(*mut self, ptr: *u8) => void;
}

marker SystemAllocator : GlobalAllocator {
    exposed alloc(*mut self, size: usize) => *u8 { ... }
    exposed free(*mut self, ptr: *u8) => void { ... }
}
```

`marker` declares a type that carries **no data at all** — the
zero-sized-type answer to the "stateless singleton implementing a spec"
pattern (a global allocator with no fields being the motivating case).
`SystemAllocator` above can be constructed (`SystemAllocator {}`), have its
address taken, implement specs, and be used through ordinary pointers and
`spec *GlobalAllocator` dynamic dispatch — everything a struct instance
can do — but declares no storage of its own, and `sizeof<SystemAllocator>`
is `0`.

## Grammar: never has fields, structurally

```
marker Name<T, ...> : Spec1, Spec2 {
    method(*self) => T { ... }
}
```

A `marker` body has no field-list section at all — not "zero fields
happen to be allowed," but no `{ field: Type; }` syntax is even reachable
inside one. `marker Foo { x: i32; }` is a parse error (`x: i32;` falls
through into the method-parsing loop, which rejects it as an invalid
function), not a value silently accepted and only rejected later during
analysis. This mirrors `void`, which likewise has no constructible
internal shape.

Everything else about a `marker` declaration is identical to a struct's:
generics, an `implements` clause, and a method list, resolved by the
exact same machinery (`Analyzer::signature_of_struct`,
`resolve_implements_clause`, method dispatch, generics instantiation,
dead-code tracking, spec/vtable coercion). A `marker` *is* a struct with
zero fields under the hood (`ResolvedType::Struct`, with
`ResolvedStructType::is_marker` set) — not a separate item kind — since
nothing about any of that machinery actually depends on having at least
one field.

## `struct`/`union` must hold real data

```
struct Empty {}     # error: 'Empty' has no sized fields
union Empty {}       # error: 'Empty' has no sized fields
```

A `struct`/`union` whose fields all resolve to zero-sized types — an
empty field list, or a field list where every field is itself zero-sized
(transitively) — is rejected outright (`AnalysisErrorKind::
ZeroSizedAggregate`), pointing at `marker` as the fix. The check is
against the type's own fully-flattened leaf list (`layout::
is_zero_sized`), not a bare field count, so it also catches:

- A struct whose only field is itself another zero-sized type
  (`struct Wrapper { m: SomeMarker; }`).
- A **generic** struct/union that only becomes zero-sized for one
  particular instantiation (`struct Wrapper<T> { x: T; }` instantiated at
  `T = SomeMarker`) — the check re-runs once per distinct
  `(item, type_args)` instantiation, the same as every other part of
  `signature_of_struct`/`signature_of_union`, so this needs no dedicated
  "also check generic instantiations" code path.

**Enums need no equivalent check.** An enum value is always
`[tag][header][dynamic][payload]`, and the tag is unconditionally the
first leaf regardless of variant or field count (`layout::
enum_prefix_layout`) — an enum can never be zero-sized, zero-variant or
not, so there's nothing to reject. A zero-variant enum is a different,
legitimate concept anyway (an *uninhabited* type — no valid value ever
exists — rather than a zero-*sized* one, which has exactly one).

## `void` is the one builtin marker

`void` already satisfies every property a `marker` type has — zero
leaves, legal in ordinary variable/parameter/field positions, no fields,
no specs — and predates `marker` in this compiler (`ResolvedType::Void`,
already relied on by `if`/block fallback typing and `=> void` functions).
`marker` doesn't change `void`'s own representation; it's simply the new
user-facing way to declare *additional* zero-sized types. `void` remains
the one example that implements nothing.

## Addresses: real, not a synthesized sentinel

Taking the address of a zero-sized value never produces a null or
otherwise-synthesized pointer — it's always a real, valid address,
exactly where ordinary layout would place it:

```
struct HasMarkerField {
    exposed m: SomeMarker;
    exposed x: i32;
}

hv : HasMarkerField;
&hv.m == &hv.x   # true -- `m` contributes zero bytes, so `x` starts
                 # at exactly the offset `m` would have ended at
&hv == &hv.m     # true, for the same reason
```

This falls straight out of the existing field-offset machinery
(`layout::place_field`/`layout_fields`) with no new code: a zero-size
field simply never advances the running offset. The same is true for
array elements, for a `comp`-evaluated marker value promoted via `&` (see
below), and for a marker's data pointer under dynamic dispatch (see
"Dynamic dispatch"). None of these needed new codegen — a marker is an
ordinary `ResolvedType::Struct` value with an empty leaf list, a shape
codegen already handled correctly before `marker` existed.

**This also holds for two independent top-level local variables**, and
is guaranteed, not incidental:

```
zst := SomeMarker {};
next_to_zst := 123;
&zst == &next_to_zst   # true
```

A function's own non-parameter locals are laid out by
`layout::locals_layout` — the exact same `layout_fields` call a struct's
fields go through, just applied to `MirBody::locals[arg_count..]` instead
— into **one combined stack slot per function** (`Codegen::frame_slot`),
each local addressed at its own precomputed byte offset within it, rather
than each local getting its own independently-placed Cranelift stack
slot. This is deliberately the *only* place a function's stack frame is
laid out: because the offset math lives in the backend-agnostic `layout`
module rather than in Cranelift-specific code, this is a genuine
cross-backend guarantee (any future backend calling the same function
gets the identical answer), not an accident of whichever backend's own
allocator happens to be compiling a given function — exactly the same
guarantee a zero-sized struct field already had, extended to locals by
reusing the same mechanism rather than inventing a second one.

## By-value parameters, including `self`, cost nothing

```
do_thing<T>(value: T) => i32 { ... }

do_thing(SomeMarker {});   # `value`'s own by-value leaves: none
```

Every by-value parameter is already passed as its flattened leaf list
(the same convention an ordinary struct-by-value parameter uses); a
zero-leaf value already contributes zero SSA values to that list. This
isn't a special case for `marker` and isn't an optimization gated by
`-O` — it's the literal calling-convention representation, so it's
unconditional at every optimization level, and it already covers `self`:
a `self`-by-value method on a marker works exactly like any other
by-value parameter, and a `*self`/`*mut self` method works exactly like
any other pointer-receiver method (a marker's `self` is a real address,
not a shared/null sentinel, so there's no restriction on `*mut self`
either — unlike a hypothetical rodata-backed singleton, a marker's
storage is real, if zero-byte, per-instance storage).

## Dynamic dispatch

```
obj : spec *GlobalAllocator = &some_system_allocator;
obj.ptr       # the real address of `some_system_allocator`, never null
obj.vtable    # the ordinary, content-deduplicated vtable, exactly like
              # any other implementor's
```

Coercing a marker to a `spec *Spec` fat pointer needs no special
handling: `SpecCoerce` already takes an ordinary address (`&Concrete`) as
its data pointer, and a marker's address is an ordinary address like any
other struct's — just one backed by zero bytes. `.ptr`/`.vtable` (see
[specs](08-specs.md)'s "Reading the two leaves directly" section) report
exactly what they would for any other implementor.

## `comp`-evaluated marker values

A `comp`-bound marker value is promoted the same way any other `comp`
value is when an address is actually needed (`&`, a `*self`-receiver
method call, range slicing — see [compile-time
evaluation](19-compile-time-evaluation.md)'s "Taking the address of ...
a `comp` binding" section): through `ConstValue::Ref` and
`Codegen::const_blobs`'s content-addressed rodata emission. A promoted
marker's blob is zero bytes; `cranelift_module`'s data-definition path
already accepts an empty payload without complaint.

## Caveats

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
