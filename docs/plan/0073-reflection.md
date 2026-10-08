# core::reflection v1: `typeinfo<T>()`

## Task Description
- **Deliverable:** a new `core::reflection` module (`runtime/core/reflection.omg`) declaring the reflection data types, a compiler-implemented `exposed typeinfo<T>() => *TypeInfo;`, and `meet Eq for TypeInfo`. `typeinfo<T>()` returns a pointer to an immutable, compiler-generated description of `T`. That description can be evaluated under `comp`, and its static tables are emitted only when runtime code reaches them.
- **Purpose:** table-driven serialization and explicit runtime type information, without hidden cost. Nothing is emitted or attached to values unless the program uses `typeinfo`. This keeps embedded and freestanding builds unaffected.
- **Chosen direction:**
  - `typeinfo` is a bodyless core declaration whose body the compiler supplies, using the same mechanism as `core::volatile` (`omega_analyzer::compiler_functions`). This means no parser or HIR changes, no hardcoded lookup of a core type by path, and ordinary imports, visibility and `comp` calls.
  - Tables are built lazily from a symbolic constant, `ConstValue::Reflected`, so recursive types such as `struct Node { next: *Node; }` work. `ConstValue::Ref` is an owned tree (`resolved_type.rs:636`) and cannot express cycles.
  - Runtime tables are one WeakODR, hidden-visibility global per type, emitted on demand like vtables (`omega-codegen/src/llvm/vtable.rs`).
  - Identity is structural, implemented in Omega by `Eq for TypeInfo`. Named types compare by kind, declaration path (which starts with the package), name and generic arguments. Unnamed types compare by their components.
- **Rejected alternatives:**
  - *`typeof<T>` name:* it reads as "the type of T" and clashes with C23/Zig's expression-type meaning.
  - *A `typeinfo<T>` keyword expression like `sizeof`:* it needs five crates of syntax plumbing plus a hardcoded lookup of `TypeInfo`.
  - *A mangled-name identity string:* mangling lives in `omega-mir`, so the analyzer and `comp` couldn't produce it.
  - *Pointer identity as the guarantee:* each `dlopen` image has its own copy of the tables.
  - *`u32`-only tags:* they break C layout matching.
  - *A typeinfo slot in every vtable:* a hidden cost.
  - *A `Type` wrapper handle:* a second name for the same thing.

## Technical Details
- **Initial context boundary:**
  - `compiler/omega-analyzer`: `compiler_functions.rs`, `analysis/items/bodies.rs`, `comp_eval.rs`, `layout.rs`, `resolved_type.rs`.
  - `compiler/omega-mir/src/mangle.rs` (next to `vtable_symbol`), `mangle/semantic.rs`, and the MIR const path only if `ConstValue` needs it.
  - `compiler/omega-codegen/src/llvm/constant.rs` and `vtable.rs`.
  - `runtime/core/`.
  - Docs: `docs/language/volatile.md` (the compiler-function precedent), `docs/language/compile-time-evaluation.md`, and `docs/architecture/types-layout-and-const-eval.md` / `mir-and-codegen.md` (the `core::volatile` paragraph near line 321).
- **Core API contract** (`runtime/core/reflection.omg`; all items `exposed`; field names are part of the contract):
  ```omega
  typeinfo<T>() => *TypeInfo;            # bodyless, compiler-implemented

  struct TypeInfo { name: *str; path: *[]*str; generic_args: *[]GenericArg;
                    size: usize; align: usize; kind: TypeKind; }
  enum GenericArg { Type { type: *TypeInfo; }, Comp { type: *TypeInfo; bits: u64; }; }
  struct FieldInfo { name: *str; offset: usize; type: *TypeInfo; visibility: Visibility; }
  enum Visibility { Hidden, Shared, Exposed; }
  enum Primitive { Void, Never, Bool, Char, I8, I16, I32, I64, ISize,
                   U8, U16, U32, U64, USize, F32, F64; }
  struct VariantInfo { name: *str; tag: u64; prototype: *u8; fields: *[]FieldInfo; }
  enum TypeKind {
      Primitive { primitive: Primitive; },
      Pointer { pointee: *TypeInfo; mutable: bool; },
      UnknownSizeArray { element: *TypeInfo; mutable: bool; },   # *[?]T
      Array { element: *TypeInfo; length: usize; },              # [N]T
      Slice { element: *TypeInfo; mutable: bool; },              # *[]T
      Str { mutable: bool; },                                    # *str
      Struct { fields: *[]FieldInfo; },
      Union { fields: *[]FieldInfo; },
      Marker,
      Enum { tag: FieldInfo; header: *[]FieldInfo; shared: *[]FieldInfo; variants: *[]VariantInfo; },
      AnonymousEnum { tag: FieldInfo; members: *[]VariantInfo; },
      Function,      # reserved: opaque in v1 (size/align/name only)
      SpecObject;    # reserved: opaque in v1
  }
  ```
  Each field above is written `exposed name: T;`. Adjust only the syntax so it compiles, not the shape. Before inventing a field name, check that `type` works as a field name (it is a contextual keyword, `lexical-structure.md:37`).
- **Table contents (semantics to implement and document):**
  - **Named types** (struct, union, marker, enum): `name` is the declared name without generic arguments; `path` is `module_path` (it begins with the package); `generic_args` follows declaration order.
  - **Unnamed types:** `name` is the analyzer's `Display` spelling, `path` is empty, and `generic_args` is empty.
  - **Generic `comp` arguments:** `bits` holds the `CompScalar` value truncated to the parameter type's width (two's complement). `bool` is 0 or 1; `char` is its code point.
  - **Size, alignment, offsets** come from `layout.rs` for the compilation target: `total_bytes`, `type_alignment`, `struct_layout`/`field_byte_offset`, `enum_header_offset`/`enum_dynamic_field_offset`/`enum_body_field_offset`. Every offset is relative to the start of the value.
  - **Hidden fields** are included, carrying their `ResolvedField.visibility`.
  - **Enum `tag`** is a FieldInfo named `tag`, with the enum's real `tag_type` and its offset.
  - **Each variant's `tag`** is `ResolvedEnumVariant.tag` as raw `u64` bits.
  - **Each variant's `prototype`** points to `sizeof<Enum>` bytes. The tag and header values (`header_values`) are written in; every other byte is zero.
  - **Anonymous enums:** each member is a `VariantInfo` with a single body field (`value`, the member type), name = the member's display spelling, and a prototype holding only the tag.
  - **Refined enum types** (`Enum { variant: Some(_) }`, `AnonymousEnum { variant: Some(_) }`) describe their parent type, because refinement never changes storage.
- **Affected files/symbols:**
  - `compiler_functions.rs`: generalize `CompilerFunction` from the single `MODULE` constant to a module per function, and add `TypeInfo`. The declaration-shape check for `typeinfo`: exactly one unbounded type generic with no default, zero params, return type `*TypeInfo` (a syntactic `Type::Pointer(Named(TypeInfo), false)`), bodyless, exposed, no annotations. Make `MalformedCompilerFunction`'s message (`error/kind.rs:1596`) name the right module instead of hardcoding `core::volatile`.
  - `analysis/items/bodies.rs::check_compiler_function_body`: currently assumes a `location` parameter. Branch per function. For `TypeInfo`, resolve the instance's `T` and the body tail is `CheckedExpr::Const(ConstValue::Reflected(Reflected::TypeInfo(T)))`, typed as the declared return type.
  - `resolved_type.rs::ConstValue`: add **one** variant, `Reflected(Reflected)`, with `enum Reflected { TypeInfo(ResolvedType), VariantPrototype { r#type: ResolvedType, variant: usize } }`. Either one is the *address* of compiler-synthesized immutable static data, and it may appear only where a pointer value is expected. Derived `PartialEq` makes `comp` pointer `==` on two TypeInfo references mean "same type". Every exhaustive `ConstValue` match needs an arm; find them with `grep -rn "ConstValue::Ref" compiler`.
  - New `compiler/omega-analyzer/src/reflection.rs`: the single builder, `pub fn type_info_value(described: &ResolvedType, info_type: &ResolvedType, pointer_bytes) -> ConstValue`. It returns the `ConstValue::Struct` for one table. Nested `*TypeInfo` fields are `Reflected::TypeInfo(..)` leaves and prototypes are `Reflected::VariantPrototype` leaves, so building is one level deep and never recurses into referenced types. It reads field order and nested type shapes (`TypeKind`, `FieldInfo`, …) from `info_type`'s resolved fields by name, so it never hardcodes positions. A missing name is an internal compiler error, which is acceptable because core is trusted. Normalize refined enums here.
  - `comp_eval.rs`: in the `CheckedProjection::Deref` arm (~line 1155), handle `Reflected::TypeInfo(t)` by calling `reflection::type_info_value(t, pointee, target pointer bytes)`. Dereferencing a `VariantPrototype` under `comp` fails with `CompErrorKind::Unsupported("reading a variant prototype")`. It is a byte image, and `comp` has no byte-level reinterpretation.
  - `omega-mir/src/mangle.rs`: add `typeinfo_symbol(&ResolvedType) -> Symbol` and `variant_prototype_symbol(&ResolvedType, usize) -> Symbol` next to `vtable_symbol`. Unnamed types use the existing structural-type owner path model (`symbol-mangling.md`). Names must be deterministic across compilation units, which is what makes WeakODR deduplication work.
  - `omega-codegen/src/llvm/constant.rs`: add a `Reflected` arm to `emit_const_value`, `write_const_element` (push a reloc), and `hash_const_element` (hash the symbol string, never the contents, since contents can be cyclic). Add a `typeinfo_global(described, info_type)` cache keyed by symbol, modeled on `vtable_for`: **insert the declared global into the cache before building its initializer** so cycles resolve. The initializer is `build_const_blob` over `reflection::type_info_value(..)`. Prototype globals are a zero-filled `total_bytes(enum)` blob, with tag and header values written at their layout offsets. Both kinds use WeakODR linkage, hidden visibility, `constant`, and the per-symbol `.rodata.<symbol>` section (except on macOS), same as vtables.
  - `runtime/core/reflection.omg`: the declarations above plus `meet Eq for TypeInfo`. It compares the `kind` discriminant. Named types compare `path`, `name` and `generic_args` element-wise, recursing through `*TypeInfo` with an address-equality shortcut on those nested pointers. Unnamed types compare their components (pointee/element/length/mutability; anonymous-enum members in order).
- **Interfaces/invariants:**
  - Zero cost unless used: no table, string or prototype exists unless a reachable `Reflected` constant is emitted. `comp`-only use emits nothing.
  - Type tables never contain function addresses. Spec-conformance listing is permanently out of scope.
  - One builder (`reflection.rs`) feeds both `comp` and codegen. Do not duplicate table construction in codegen.
  - `typeinfo` instances are ordinary generic instances with weak linkage, like volatile ones.
  - `Reflected` is only ever produced by the compiler-implemented body and by the builder. No user syntax creates it.
- **Out of scope:** `Function`/`SpecObject` payloads, reflection of functions/specs/methods, user annotations on fields, accessing fields by `comp` index, `spec Typed`/`Any`, and connecting `==` to `Eq`.
- **Risks/open questions (stop and escalate):**
  - Generic instances compared under `comp`. `ResolvedStructType` equality is `id` only (`resolved_type.rs:303`). Verify that `Pair<i32>` and `Pair<u8>` get different ids. If they don't, `comp` `==` and the codegen cache would merge them, and the fix needs a design decision.
  - The `Display` spelling turns out not to be canonical for unnamed types (for example, it varies with aliases).
  - Any need to extend the MIR const representation beyond carrying `ConstValue` through unchanged.

## Implementation Plan
1. **Generalize compiler functions.** Give `CompilerFunction` per-function modules and fix the `MalformedCompilerFunction` message. Update `compiler_functions/tests.rs`. No behavior change for volatile.
2. **Add `ConstValue::Reflected`** and the `Reflected` enum. Add arms everywhere `ConstValue` is matched. Codegen arms may be `unreachable!` until step 6, provided nothing produces the value yet.
3. **Add `reflection.rs`** with `type_info_value`, covering every kind in the contract.
4. **Add `runtime/core/reflection.omg`** (types, bodyless `typeinfo`, `Eq`). Classify `typeinfo` in `compiler_functions.rs` and build its body in `check_compiler_function_body`.
5. **`comp` support:** handle the deref of `Reflected` in `comp_eval.rs`. `comp` reads of `size`, `align`, fields and kinds, plus `Eq`, should now work.
6. **Runtime emission:** add the mangle symbols, then `typeinfo_global` and the prototype globals, then the `Reflected` arms in `constant.rs`.
7. **Docs:**
   - New normative chapter `docs/language/reflection.md`: contract, table semantics, identity via `Eq`, on-demand emission, `comp` behavior including the prototype limitation. Add it to `docs/language/README.md`, and make `volatile.md` / `compile-time-evaluation.md` cross-reference compiler-implemented declarations.
   - `docs/architecture/mir-and-codegen.md`: generalize the volatile compiler-function paragraph (line ~321) and describe the table/prototype globals.
   - `docs/architecture/types-layout-and-const-eval.md`: describe `ConstValue::Reflected` and its lazy expansion.
   - `docs/architecture/runtime-and-platform.md` lines 29/37: list `core::reflection` as compiler-privileged, replacing "the two bodyless `core::volatile` declarations".
   - Add a quick-reference entry in `docs/guide/quick-reference.md`.

## Testing
- **Rust tests:**
  - `compiler_functions/tests.rs`: `typeinfo` classifies, and a malformed `typeinfo` declaration is rejected.
  - Analyzer unit tests for `reflection.rs`: offsets and sizes for a packed struct and an `@layout`-aligned one, an enum with `u8` tag + header + shared field, an anonymous enum, a refined enum normalizing to its parent.
- **Conformance `tests/t51_reflection/`** (proves `docs/language/reflection.md`):
  - Runtime: print `name`/`size`/`align`/field names/offsets/visibilities of a struct with a hidden field. Walk a recursive `Node` through `*TypeInfo`. Walk an enum: find the variant by reading the tag at `tag.offset`, then rebuild a value from `prototype` plus body-field writes and print it.
  - `comp`: `comp` bindings from `typeinfo<T>().size` / field offsets, and `typeinfo<Pair<i32>>()` vs `typeinfo<Pair<u8>>()` equality under `comp` and at runtime.
  - `Eq` true for the same type reached through different paths (a field's `type` vs a direct `typeinfo`), and false for differing generic or `comp` args (`Buffer<3, i32>` vs `Buffer<4, i32>`).
- **Separate compilation:** a test that calls `typeinfo<SameType>()` from two source files, so tables are emitted twice, showing that it links and that `Eq` holds. `testing-and-validation.md` says how multi-file packages are expressed.
- **Negative `tests/t51b_reflection_errors/`:** dereferencing `prototype` under `comp`. Check the exact `expected.stderr` unsupported-operation diagnostic.
- **Zero-cost check:** an object built from a program that does not use `typeinfo` contains no `typeinfo` symbols. A program using it only under `comp` emits none either. Check with a codegen/`omgc` component test inspecting emitted symbols, wherever existing symbol-inspection tests live (find them by searching for `vtable` in the tests).
- **Regression:** `t48_volatile`, `t48b_volatile_errors`, `t48c_function_without_body`, `t34_comp_generics`, `t37_layout_alignment`, then `just test-all`.
