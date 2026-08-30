# Structs and unions

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

## Structs

```omega
struct Point<T> {
    exposed x: T;
    exposed y: T;

    exposed origin() => Point<T> {
        Point<T> { x = 0; y = 0; }
    }

    exposed translate(*mut self, dx: T, dy: T) => void {
        self.x += dx;
        self.y += dy;
    }
}
```

A struct is a nominal aggregate with an ordered field list followed by zero or more methods. Fields use declaration syntax `name: Type;`; struct-literal fields use `name = expression;`.

```omega
p := Point<i32> { x = 10; y = 20; };
```

A struct literal must initialize every field exactly once. There is no partial-initialization or spread syntax.

Fields and methods may be hidden, `shared`, or `exposed`; see [`visibility.md`](visibility.md). Generic parameters follow the struct name and obey [`generics.md`](generics.md); a method may also declare generic parameters of its own.

A method with a receiver is an instance method. A method without a receiver is static. The two live in separate associated-function namespaces -- `Type::method` selects a static, `Type::self::method` selects a member -- so they may share a name; see [`functions.md`](functions.md#associated-function-namespaces).

A struct whose instantiated value representation is zero-sized is invalid; use `marker` for nominal zero-sized types.

## Unions

```omega
union Value {
    exposed as_i32: i32;
    exposed as_f32: f32;
}
```

A union is a nominal aggregate whose fields overlap the same storage. Its size is sufficient for its largest field. All fields begin at offset zero.

A union literal selects exactly one field:

```omega
v := Value { as_i32 = 42; };
```

Unions may have methods and generics under the same rules as structs. A zero-sized union is invalid.

`@layout` is not accepted on unions in the current language. Their current layout is packed with alignment 1; see [`annotations-and-sizeof.md`](annotations-and-sizeof.md) and [`../issues/design-debt.md`](../issues/design-debt.md) for target-safety caveats around the current packed layout model.

## Layout and `sizeof`

Struct layout, union layout, `@layout`, and `sizeof<Type>` are specified in [`types-and-primitives.md`](types-and-primitives.md) and [`annotations-and-sizeof.md`](annotations-and-sizeof.md).
