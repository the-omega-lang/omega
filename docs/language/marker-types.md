# Marker types

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

A `marker` declares a nominal zero-sized type.

```omega
marker NullSink {
    exposed write(*mut self, bytes: *[]u8) => void { ... }
    exposed flush(*mut self) => void { ... }
}
```

A marker can be constructed with `{}`, used as a generic argument, have methods, conform to specs, have its address taken, and participate in dynamic dispatch. Its value size is exactly zero:

```omega
x := NullSink {};
sizeof<NullSink>   # 0
```

## Grammar and members

```ebnf
marker = [ visibility ], "marker", identifier,
         [ generic-parameters ],
         "{", { method }, "}" ;
```

Markers cannot declare fields. A field-shaped declaration inside a marker body is invalid syntax. Methods and generic parameters otherwise follow the same rules as other nominal types.

## Zero-sized aggregates

An ordinary `struct` or `union` must contain non-zero-sized instantiated storage. If all of its fields recursively contribute zero bytes, the type is invalid. Use a `marker` when a nominal type intentionally carries no data.

Enums are not subject to this rule because every enum value carries a discriminant/tag and therefore is not zero-sized.

`void` is the built-in zero-sized type; `marker` is the user-declarable nominal form.

## Address semantics

A marker value occupies zero bytes, but a marker **place** still has an address determined by ordinary object layout. Implementations must not replace every marker address with one universal null/sentinel pointer.

A zero-sized field does not advance the containing object's offset, so adjacent zero-sized and non-zero-sized fields may have equal addresses:

```omega
struct HasMarkerField {
    exposed m: SomeMarker;
    exposed x: i32;
}

hv : HasMarkerField;
&hv.m == &hv.x   # may be true because m consumes zero bytes
```

The same place/address rule applies to marker locals, fields, array elements, values materialized from compile-time data, and data pointers used for dynamic dispatch.

## Calls and dynamic dispatch

Passing a marker by value transfers no payload bytes, but otherwise follows normal by-value call semantics. Pointer receivers operate on the marker place's real address.

A marker may conform to a spec and be coerced to `*spec Spec` / `*mut spec Spec` in the same way as any other concrete type. The data pointer identifies the concrete marker place; the dispatch metadata follows the ordinary dynamic-spec-object rules in [`specs-and-conformance.md`](specs-and-conformance.md).
