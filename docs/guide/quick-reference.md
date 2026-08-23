# Omega quick reference

A compact syntax guide for writing `.omg` source without guessing from Rust/C/C++. The forms below are taken or minimally adapted from Omega source already present under `examples/` and `runtime/` in this repository.

This is **not** the normative language definition. For exact rules, follow the links into [`../language/`](../language/).

## The biggest Rust false friends

| Intent | Omega | Do not guess |
|---|---|---|
| comment | `# text` | `// text` |
| function | `add(a: i32) => i32 { ... }` | `fn add(...) -> i32` |
| inferred binding | `x := value;` | `let x = value;` |
| mutable binding | `mut x := value;` | `let mut x = value;` |
| public visibility | `exposed` | `pub` |
| interface | `spec` | `trait` / `interface` |
| implementation | `conform T to S { ... }` | `impl S for T` |
| cast | `<u64>x` | `x as u64` |
| macro invocation | `println$("hi");` | `println!("hi")` |
| type/name alias | `alias Short = Long;` | `type Short = Long;` |
| fixed array type | `[64]u8` | `[u8; 64]` |
| struct literal field | `x = 1;` | `x: 1,` |

When editing `.omg`, do not infer missing syntax from Rust simply because the languages look similar.

## Comments and imports

```omega
# A line comment.

import std::fmt::Display;
import self::simplemodule;
import self::mymodule::thing::something2;
import root::simplemodule;
import super::sibling;
```

See [`../language/lexical-structure.md`](../language/lexical-structure.md) and [`../language/modules-and-imports.md`](../language/modules-and-imports.md).

## Functions

There is no `fn` keyword. Return types use `=>`.

```omega
add(a: i32, b: i32) => i32 {
    a + b
}

say(message: *str) => void {
    println$(message);
}

early(x: i32) => i32 {
    if x < 0 { return 0; }
    x
}
```

The final expression of a block can provide its value.

## Bindings and mutability

```omega
x := 10;
mut y := 20;

name: *str = "Omega";
mut count: i32 = 0;
mut buffer: [64]u8;
```

Mutation is explicit:

```omega
count = count + 1;
count += 1;
++count;
```

## Primitive/literal examples

```omega
signed := -12;
hex := 0x7Fu32;
flag := true;
letter := 'A';
text := "hello";
bytes := b"raw bytes";
```

Common types visible in the repository include:

```text
i8 i16 i32 i64 isize
u8 u16 u32 u64 usize
f32 f64
bool char str void never
```

## Pointers, arrays, slices

```omega
value: i32 = 10;
p: *i32 = &value;

mut other: i32 = 20;
pm: *mut i32 = &mut other;

mut bytes: [64]u8;
view: *mut []u8 = &mut bytes[0..];
```

Observed type spellings:

```text
*T          pointer
*mut T      mutable pointer
[N]T        fixed-size array
*[?]T       unknown-size array pointer
*[]T        slice pointer
*mut []T    mutable slice pointer
```

See [`../language/types-and-primitives.md`](../language/types-and-primitives.md) and [`../language/strings-casts-arrays-and-slices.md`](../language/strings-casts-arrays-and-slices.md).

## Casts and `sizeof`

```omega
wide := <u64>small;
ptr := <*[]u8>raw;
bytes := sizeof<usize>;
```

## Structs

```omega
struct Vec2 {
    exposed x: i32;
    exposed y: i32;

    exposed origin() => Vec2 {
        Vec2 { x = 0; y = 0; }
    }

    exposed translate(*mut self, dx: i32, dy: i32) => void {
        self.x += dx;
        self.y += dy;
    }
}

p := Vec2 { x = 10; y = 20; };
```

Receiver forms that exist in repository source:

```omega
method(self) => T { ... }
method(mut self) => void { ... }
method(*self) => T { ... }
method(*mut self) => void { ... }
```

## Unions

```omega
union Value {
    exposed as_i32: i32;
    exposed as_u64: u64;
}
```

## Marker types

```omega
marker UnitLike { }
```

Markers are the source-level zero-sized user type form. See [`../language/marker-types.md`](../language/marker-types.md).

## Enums

Simple enum:

```omega
enum Ordering {
    Less,
    Equal,
    Greater;
}
```

Generic enum with variant body data:

```omega
enum Optional<T> {
    None,
    Some {
        exposed value: T;
    };
}

some := Optional<u32>::Some { value = 42u32; };
none := Optional<u32>::None;
```

Enums may also have header/shared fields and methods; see [`../language/enums-and-pattern-matching.md`](../language/enums-and-pattern-matching.md).

## `if`, `match`, and loops

```omega
label := if x > 0 { "positive" } else { "non-positive" };

match value {
    0..=9 => { println$("digit"); },
    .. => { println$("other"); },
} else {
    println$("unmatched");
}

while condition {
    work();
}

loop {
    if done { break; }
}

for mut i := 0; i < n; ++i {
    work(i);
}

for c in 'a'..='z' {
    work(c);
}
```

## Ranges

Forms used by repository source include:

```omega
'a'..='z'     # inclusive end
'0'..<'9'     # exclusive end
0..           # open end, e.g. slicing
```

See [`../language/iteration-and-ranges.md`](../language/iteration-and-ranges.md).

## `defer`

```omega
defer {
    println$("leaving function");
}
```

`defer` is function-exit cleanup; exact behavior is specified in [`../language/functions.md`](../language/functions.md).

## Inline assembly

```omega
mut x : i32 = 0;
y := 20i32;
asm(reg(&mut x, "rcx"), reg(y)) => {
    add $y, 22
    mov dword ptr [$x], $y
}
```

`reg(expr)` is a by-value snapshot with no implicit writeback; mutate Omega storage explicitly with `reg(&mut x)`. `const(NAME)` substitutes a `comp` value as literal assembler text. The body is raw backend assembly (X86/X86-64 uses Intel syntax), not Omega syntax -- full rules, including `$$`/`clobber`/dialect/optimization-opacity, are in [`../language/inline-assembly.md`](../language/inline-assembly.md).

A `@naked` function's body is exactly one `asm` statement that owns the whole function, with no Omega-generated prologue/epilogue:

```omega
@naked
get_magic() => i32 {
    asm() => {
        mov eax, 123
        ret
    }
}
```

See [`../language/functions.md`](../language/functions.md#naked-functions).

## Generics

```omega
struct MyNode<T> {
    exposed value: T;
    exposed next: *MyNode<T>;
}

identity<T>(value: T) => T {
    value
}

bounded<T: Animal + Display>(value: T) => void {
    ...
}
```

Static member access uses `::`:

```omega
writer := BufWriter<Stdout>::new(&mut stdout, &mut buf[0..]);
```

## Specs and conformance

A `spec` is Omega's interface-like construct:

```omega
exposed spec Iterator<T> {
    next(*mut self) => Option<T>;
}
```

Implementation is written with `conform`:

```omega
conform char to Ord {
    compare(*self, other: Self) => Ordering {
        ...
    }
}
```

Blanket conformance is generic:

```omega
conform<T: Ord> T to Eq {
    ...
}
```

A conjunction of specs is written directly at the type where it's needed, not declared separately:

```omega
use_both<T: A + B>(value: *T) => void { ... }
speak(animal: *spec A + B) => void { ... }
```

See [`../language/specs-and-conformance.md`](../language/specs-and-conformance.md).

## Primitive extension blocks

Repository runtime source adds methods/conformances to compiler primitive types with `primitive`:

```omega
primitive char {
    exposed is_ascii(*self) => bool {
        <u32>*self <= 0x7Fu32
    }
}

primitive<T> []T {
    exposed is_empty(*self) => bool {
        self.size == 0
    }
}
```

## Gaps and glue

A capability required from the platform/runtime can be declared as a `gap`, and supplied by `glue`:

```omega
gap GlobalAllocator {
    alloc(size: usize) => *mut u8;
}

glue core::platform::GlobalAllocator {
    alloc(size: usize) => *mut u8 {
        ...
    }
}
```

See [`../language/gaps-and-glue.md`](../language/gaps-and-glue.md).

## Compile-time evaluation

```omega
comp RESULT := comp add(10, 20);
comp POINT := comp Point { x = 1; y = 2; };
```

See [`../language/compile-time-evaluation.md`](../language/compile-time-evaluation.md) before relying on what the evaluator accepts.

## Annotations

This layout spelling is present in repository Omega source:

```omega
@layout(pack = sizeof<usize>, align = sizeof<usize>)
struct Header {
    ...
}
```

Other supported annotations are normative in [`../language/annotations-and-sizeof.md`](../language/annotations-and-sizeof.md).

## Macros

Definition:

```omega
macro sum_macro($a: expr, $b: expr) => {
    ($a) + ($b)
}
```

Invocation uses `$`, not `!`:

```omega
value := sum_macro$(10, 20);
println$("value = {}", value);
```

Variadic macro parameters appear in runtime source as `$args: expr...`; see [`../language/macros.md`](../language/macros.md) for repetition syntax and hygiene.

## Aliases

`alias` gives an existing declaration a second name. It is top-level only, and
never creates a new type, function, or symbol:

```omega
alias Count = i32;
alias IntPair = Pair<i32, i32>;
alias Keyed<V> = Pair<*str, V>;
alias AB = spec A + B;
alias Dyn = *spec B + A;
alias plus = add;             # functions, overload sets, macros, modules too
exposed alias Public = Hidden;   # deliberate re-export
```

The right-hand side is type syntax (never an expression) and is resolved in the
module where the alias is written. See [`../language/aliases.md`](../language/aliases.md).

## Visibility

```omega
exposed public_api() => void { }
shared package_api() => void { }
hidden_by_default() => void { }
hidden also_hidden_by_default() => void { }
```

`hidden` is spelled out explicitly only where it changes something -- e.g. narrowing a spec member below its spec's own visibility; writing it anywhere it merely restates the default is a suppressible warning. See [`../language/visibility.md`](../language/visibility.md) for the distinction between item/member visibility and `reveal`.
