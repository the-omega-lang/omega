# Specs and conformance

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

A `spec` is Omega's named interface/trait construct. Specs define function requirements, can provide default bodies, can be used as generic bounds, and can be used for static or dynamic dispatch.

## Declarations

```omega
exposed spec Animal {
    kind(*self) => AnimalKind;
    make_sound(*self) => *str;

    is_dog(*self) => bool {
        self.kind().tag == AnimalKind::Dog.tag
    }
}
```

A function without a body is required. A function with a body is a default implementation that a conformance may use or replace.

`Self` denotes the concrete implementing type. Spec receiver parameters must use `*self` or `*mut self`; by-value `self`/`mut self` is invalid in a spec declaration.

A spec declares only its own requirements. There is no alias mechanism: `spec AB = A + B;` is not valid Omega syntax, and a spec declaration never carries a dependency list (`spec X : A, B { ... }` is also invalid). A conjunction of specs is written directly at the type where it is needed -- see [Spec conjunctions in type syntax](#spec-conjunctions-in-type-syntax) below -- and is satisfied by conforming to each member separately; there is nothing to `conform T to` for a conjunction itself.

## Requirement identity

A spec function is identified by its declaring spec as well as its name/signature. `A::tag` and `B::tag` are distinct requirements even when their source signatures are identical, and a conjunction never merges them merely because their names match.

## Spec conjunctions in type syntax

`spec A + B + ...` is one type-level form: a non-empty, `+`-separated list of spec members. Source order carries no semantic weight -- `spec A + B` and `spec B + A` are exactly the same type, and a repeated member (`spec A + A`) collapses to the same type as `spec A`. The compiler canonicalizes a conjunction by resolving every member to its final spec declaration, normalizing its generic arguments, removing exact duplicates, and sorting what remains by fully qualified spec name (with normalized arguments breaking ties among same-named specs). Two conjunctions that canonicalize to the same shape are the same type: nothing about them differs, so there is no conversion, cast, or diagnostic for reordering one into the other.

What a `spec ...` type means depends entirely on where it appears, never on how it is spelled:

- As a **bare type** (a generic bound, or the top-level type of a function parameter), it denotes a *static* constraint -- see [`spec S` as a static-dispatch type](#spec-s-as-a-static-dispatch-type-not-behind-a-pointer).
- As the **immediate pointee of a pointer** (`*spec A + B`, `*mut spec A + B`), it denotes a *dynamic* spec object -- see [Dynamic dispatch](#dynamic-dispatch-pointer-to-spec).

A `spec ...` type appearing anywhere else (behind an array, inside another type's generic arguments, as a function type's parameter/return without being that function type's own outermost form, etc.) is not allowed; static-vs-dynamic is decided only by that one immediate structural position.

## Explicit conformance

```omega
struct Dog {
    exposed id: i32;
}

conform Dog to Animal {
    kind(*self) => AnimalKind { AnimalKind::Dog }
    make_sound(*self) => *str { "woof" }
}
```

`conform Target to Spec { ... }` declares nominal conformance.

- Every required function must be supplied exactly once unless the spec provides a default.
- Extra functions that are not requirements are invalid in the conform block.
- Inherent methods do not implicitly satisfy spec requirements.
- Conform methods do not declare their own visibility; they implement the requirement.
- Even a spec containing only defaults requires an explicit (possibly empty) conformance.

A conformance is legal when the current package owns either the target type or the spec. This is Omega's orphan/coherence rule for concrete conformances.

Two equally specific conformances for the same target, spec, and spec arguments are invalid.

### Supported conformance targets

Concrete and blanket conformance targets may be nominal user types and the supported primitive/string/slice forms described by the language. Raw pointers, function types, inline-array constructors, and dynamic spec-object types are not standalone conformance targets.

Slice conformance has a known current reachability limitation; see [`../issues/known-issues.md`](../issues/known-issues.md).

## Blanket conformances

```omega
conform<T: Numeric> T to Sum {
    sum(*self) => i32 { 0 }
}
```

A blanket conformance applies to every conformable concrete type satisfying its generic bounds. An unbounded blanket is also allowed:

```omega
conform<T> T to SomeSpec { ... }
```

Selection rules:

1. A matching concrete conformance is more specific than any blanket.
2. Between matching blankets, compare their required bound sets.
3. A strict superset of bounds is more specific than a strict subset.
4. Incomparable matching bound sets are ambiguous and must be rejected.
5. An unbounded blanket has the empty bound set and is therefore less specific than any otherwise matching bounded blanket.

A blanket may implement only a spec owned by the blanket's package. This prevents a package from claiming every foreign type for a foreign spec.

A blanket can entail another spec inside generic code. For example, if `conform<T: Ord> T to Eq` exists, a generic body bounded by `T: Ord` may rely on the derived `Eq` conformance.

## Calling conforming functions

Conforming functions are not ordinary globally inherent methods. Omega provides three qualification levels:

```omega
S::make()                 # target type supplied; valid when unambiguous
P::make(...)              # spec supplied; Self inferred where allowed
<S : P>::make(...)        # target and spec supplied explicitly
```

For an instance function, `Spec::function(receiver, args...)` takes the receiver as the first call argument:

```omega
Animal::make_sound(&dog);
```

For a receiverless requirement returning exactly `Self`, an expected result type may identify the concrete implementor:

```omega
x : Foo = Default::default();
```

If the implementing type cannot be inferred unambiguously, use `<Type : Spec>::function(...)`.

Conforming instance calls adapt their receiver according to the declared receiver form just like ordinary methods. A value may be referenced for `*self` when legal; a mutable receiver requires a mutable source. Pointer-shaped built-in values such as strings/slices do not gain an extra pointer layer merely because a spec receiver is written `*self`.

Taking a pointer to a temporary receiver may require materializing that temporary. A `*mut self` requirement cannot use a discarded immutable temporary merely to obtain mutability.

## Multiple instantiations of a generic spec

A concrete type may conform to different instantiations of the same generic spec:

```omega
spec Consumer<T> {
    consume(*self, value: T) => i32;
}

conform Multi to Consumer<i32> { ... }
conform Multi to Consumer<*u8> { ... }
```

Requirements are matched by name and full parameter signature for the particular spec instantiation. Ordinary Omega overloading rules still apply: functions cannot be overloaded solely by return type, so two required methods that differ only in return type cannot both be implemented as distinct overloads.

## Static dispatch through bounds

```omega
use_animal<T: Animal>(animal: *T) => void {
    animal.make_sound();
}

use_both<T: Animal + Dummy>(value: *T) => void {
    value.make_sound();
    value.something_else();
}
```

A bound is nominal: a concrete instantiation of `T: Animal` must have an applicable conformance to `Animal`. `T: A + B` requires both.

Methods supplied by declared/entailed bounds are in scope for generic code. If two bound specs make the same method name applicable and no unique overload can be selected, the call is ambiguous.

An unbound generic parameter is not implicitly considered to conform to arbitrary specs; however, ordinary method lookup on its concrete monomorphized instantiation may still succeed where the language permits unconstrained generic duck-typed method use. See [`generics.md`](generics.md).

## Dynamic dispatch: pointer to `spec ...`

```omega
speak(animal: *spec Animal) => void {
    animal.make_sound();
}

speak(&dog);
```

`*spec S` is parsed structurally as an ordinary pointer to the static spec type `spec S` -- the grammar does not manufacture a fat pointer. Semantic type resolution recognizes an *immediate* `Pointer(spec ...)` and turns it into a dynamic spec-object type: an immutable dynamic-dispatch object, with `*mut spec S` (or `*mut spec A + B`) as its mutable counterpart. The value is a fat pointer containing:

1. an opaque data pointer to the concrete value, and
2. a dispatch-table pointer for the requested spec shape.

Only that one immediate position is recognized this way; an ordinary `Pointer { pointee: Spec, .. }` never survives past semantic analysis behind another type constructor -- see [Spec conjunctions in type syntax](#spec-conjunctions-in-type-syntax).

A pointer to a conforming concrete value may be coerced to an expected dynamic spec-object type at ordinary coercion sites such as arguments, assignments/initializers, returns, aggregate fields, and array elements. The mutable form requires a mutable concrete pointer.

An explicit cast is also available:

```omega
obj := <*spec Animal>&dog;
```

The cast is valid only when the concrete pointee satisfies the requested spec/conjunction.

### Conjunction objects and narrowing

For a conjunction object such as `*spec A + B`, each canonical shape member retains its own section of the dispatch table, in the shape's canonical order (sorted by fully qualified spec name). If two members declare the same callable name, an unqualified dynamic call is ambiguous. A zero-method member (a marker spec) still occupies its section identity even though it contributes no slots, so members after it are unaffected by whether an earlier member happens to have methods.

Use a narrowing cast to select one already-present member:

```omega
(<*spec A>obj).tag();
(<*spec B>obj).tag();
```

Narrowing only ever selects one member already present in the source shape's canonical set; it never reconstructs an arbitrary multi-member sub-conjunction. Reordering a conjunction's own spelling (`*spec B + A` vs. `*spec A + B`) is not a narrowing cast at all -- both spellings are the same canonical type, so there is nothing to convert. Widening from a narrower dynamic object to a larger conjunction is not implicitly or explicitly fabricated, because the missing dispatch sections do not exist.

### `.ptr` and `.vtable`

Dynamic spec objects expose two pseudo-fields:

```omega
raw := obj.ptr;
vt := obj.vtable;
```

- `.ptr` is `*u8` or `*mut u8` following the dynamic object's mutability.
- `.vtable` is always an immutable `*u8`.

These access the two fat-pointer components; they are not members declared by the concrete implementor.

For the same concrete type and same resolved dynamic-spec shape, implementations must produce stable compatible dispatch metadata for calls. Do not rely on `.vtable` pointer identity across independently built program images as a language-level type-id mechanism unless the ABI explicitly guarantees that context.

Dynamic spec objects are not currently supported by `comp` evaluation; see [`compile-time-evaluation.md`](compile-time-evaluation.md).

## `spec S` as a static-dispatch type (not behind a pointer)

`spec S` as a bare type -- not the immediate pointee of a pointer -- means "some concrete type satisfying `S`" and is distinct from the dynamic `*spec S` form.

### Parameter position

```omega
consume(value: spec Animal) => void { ... }
```

Each `spec S` parameter behaves as an independent anonymous generic parameter bounded by `S`. Two such parameters are not required to have the same concrete type merely because they name the same spec.

### Return position in a spec requirement

```omega
spec ToIterator<T> {
    to_iterator(*self) => spec Iterator<T>;
}
```

Inside a spec requirement, `=> spec S` allows each implementor to choose its own concrete return type, provided that return type satisfies `S`. This is associated-type-like behavior for that function result.

A spec containing such a return requirement is **not object-safe** and therefore cannot form `*spec ThatSpec`: a single dynamic dispatch slot cannot describe an implementation-specific concrete return type.

### Return position in an ordinary function

An ordinary function definition may not return `spec S` as a hidden body-inferred concrete type:

```omega
make() => spec Animal { ... }   # invalid
```

Use an explicit generic chosen by the caller, a concrete return type, or a dynamic `*spec Animal` return as appropriate.

## Primitive extension blocks

Built-in primitive/string/slice types receive inherent methods through `primitive` blocks in the `core` package:

```omega
primitive str {
    exposed is_empty(*self) => bool { self.size == 0 }
}

primitive<T> []T {
    exposed first(*self, out: *mut T) => bool { ... }
}
```

A supported primitive target has exactly one declaration block. The block may be empty. Methods use ordinary visibility and call rules.

Primitive inherent methods and spec conformance are separate concepts. A package permitted by the conformance ownership rule may conform a primitive target to a spec independently of its `primitive` declaration.
