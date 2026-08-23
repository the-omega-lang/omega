# Semantic types, layout, target width, and compile-time values

Several cross-cutting compiler facts live in `omega-analyzer` because they are semantic/representation decisions that MIR and codegen must consume identically.

The two most important are:

- the canonical resolved type graph (`resolved_type.rs`);
- the shared aggregate/leaf layout algorithms (`layout.rs`).

Compile-time values (`ConstValue`, `comp_eval.rs`) are built against the same type/layout/target vocabulary.

## Resolved types as shared semantic data

`ResolvedType` is the post-resolution type vocabulary. It is used by:

- analyzer checking;
- driver query keys and conformance/generic matching;
- checked tree;
- MIR;
- shared ABI construction;
- codegen.

This is intentional: later stages should not translate semantic types into codegen-private competing definitions before common layout/ABI facts are decided.

### Nominal aggregate cells

Structs, enums, unions, and specs use shared reference-counted cells.

For aggregates the cell holds facts such as:

- stable HIR/synthetic identity;
- declared name/module/type args;
- resolved fields/methods/variants;
- resolved layout annotation data;
- suppression/marker metadata.

The driver can create the cell before collection completes, allowing safe recursive indirection to reference stable nominal identity. The owning signature analysis fills it exactly once.

Consumers should keep/reference the cell rather than copy a second aggregate definition into every expression.

### Structural shapes

Not every aggregate is nominal. A dynamic spec object and an anonymous enum are **structural**: they have no declaration, no `HirId`, no module, and no driver-owned cell, so their identity is the shape itself.

Both are canonicalized once — a deterministically ordered, exact-duplicate-free member list — before any equality, hash, layout, or mangling question is asked. For an anonymous enum that canonical list is a **leaf** list: `ResolvedAnonymousEnum::canonicalize` first replaces every immediate `ResolvedType::AnonymousEnum` member by its own members, recursively, so no stored member is itself an anonymous enum. Type resolution is the sole producer of the shape, so an alias or generic substitution that lands one anonymous enum inside another normalizes there and downstream phases — layout, tags, const values, pattern coverage, mangling — consume the already-flattened list and must not re-sort or re-flatten it. Ordering comes from `omega-analyzer::type_key`, the single structural identity key for a `ResolvedType`:

- it uses fully qualified nominal names plus normalized generic arguments, recursively;
- it never observes `HirId`, pointer addresses, or discovery order, so separate compilations and separate packages agree;
- it distinguishes everything `ResolvedType`'s own `PartialEq` distinguishes, so sorting by it groups equal types adjacently and deduplication is exact.

`Display` is not a substitute: it prints bare nominal names with no generic arguments, so `Vec<i32>` and `Vec<f64>` render alike.

Aggregate members use a dedicated `ResolvedField { name, type, visibility }` representation. Function parameters use the function-signature parameter representation instead. This separation is deliberate: field visibility and aggregate-member identity are semantic facts and should not be encoded in parameter-shaped tuples.

## Function types

`ResolvedFunctionType` contains resolved parameter types, return type, variadic flag, and self-mode information. It is the input to shared ABI construction.

Method receiver shape is semantically explicit by this point; codegen should not re-infer source receiver modes.

## Target vocabulary

`omega_analyzer::Target` contains the compiler-wide architecture/OS choice and provides `pointer_bytes()` / `pointer_bits()`.

Width-sensitive semantic operations must take that target information rather than assuming host width.

Examples include:

- `isize`/`usize` numeric domains;
- casts;
- `sizeof`/layout;
- compile-time arithmetic that depends on pointer width;
- ABI leaf sizing.

LLVM target triples are derived later from the shared target.

## Layout ownership

`omega-analyzer::layout` is the **one source of truth** for byte/leaf layout used by codegen.

It provides:

- scalar/aggregate flattening to abstract `Leaf`s;
- field byte offsets and positional leaf starts;
- total byte size;
- alignment;
- struct field placement/packing;
- enum prefix/payload layout;
- union storage size;
- function local-frame layout;
- stack alignment requirements.

Codegen may map a `Leaf` to its native scalar type, but it must not invent different aggregate offsets.

## Abstract leaves

A `Leaf` is a backend-neutral scalar storage component. Aggregate values are commonly represented as ordered leaf lists for registers/SSA transfer while memory access uses byte offsets from the same layout algorithm.

This dual view is why `FieldLayout` records both:

```text
byte_offsets   memory-backed field positions
leaf_starts    positions inside flattened value lists
leaves         flattened storage sequence
packed_end     byte end before whole-sequence trailing alignment
```

When explicit layout padding exists, leaf-list position and byte offset are not safely derivable from one another, so both are computed together.

## Struct layout

For a struct, field types are passed through `layout_fields` using the resolved `@layout(pack = ...)` value. The algorithm applies:

1. field's transitive alignment;
2. enclosing pack/chunk constraint;
3. field size from its leaf representation;
4. explicit padding leaves where needed.

The struct's resolved alignment/packing annotation data lives on the resolved struct cell, not in codegen.

## Local stack-frame layout

Non-parameter MIR locals are laid out through `locals_layout`, using the same field-layout machinery as an unannotated aggregate.

Codegen consumes this one result rather than re-deriving offsets.

Parameters are a distinct storage source at function entry and occupy `MirBody.locals[0..arg_count]`; codegen may materialize/spill them to memory when an address is required.

## Enum layout

An enum's storage is conceptually:

```text
tag
header fields
shared dynamic fields
variant payload region
```

The prefix is one normal field sequence. Every variant payload shares one payload start, and that start is aligned for the strongest variant-field requirement. The payload's storage size is the maximum variant body size.

Memory offsets for header/dynamic/body fields are provided by shared helpers such as:

- `enum_header_offset`;
- `enum_dynamic_field_offset`;
- `enum_payload_offset`;
- `enum_body_field_offset`.

The payload can be represented as opaque integer leaves for flattened transfer; those leaves are storage chunks, not semantic numeric values. A body field therefore has no typed leaf slice of its own: reaching one means reaching memory, so a register-held enum is spilled to a stack slot before its body byte offset is applied.

Those helpers do not take a declared enum. They take `layout::EnumView`, the layout-relevant shape shared by **named and anonymous enums** — tag type, header field types, shared dynamic field types, per-variant tag/header values/body field types, and pack/align. An anonymous enum is the degenerate case of that view: a `u16` tag holding the canonical member index, no header, no shared dynamic fields, and exactly one body field per variant holding that member.

This is the only place the two enum forms are reconciled. Codegen reads tag/header/payload facts through the same view rather than destructuring a nominal enum cell, so neither form can acquire a second representation rule.

Implicit enum tags are checked against the resolved tag type's integer domain before they become `NumberValue`s. This mirrors explicit-tag range checking and keeps the “every variant receives a representable, unique tag” invariant true even for extremely large enums. Pointer-sized integer domains use the active target's pointer width.

## Union layout

A union's byte storage is the maximum of its fields. Flattened payload chunks cover that storage so unions can pass through the same leaf machinery while field access itself remains a reinterpretation of the shared memory region.

## ABI vs layout

Layout and calling convention are separate owners:

- `omega-analyzer::layout` answers **how a value is represented/laid out**;
- `omega-codegen::abi` answers **how function parameters/results travel across a call boundary** using that representation.

See [`abi-and-representation.md`](abi-and-representation.md).

## Compile-time values

`ConstValue` is the compiler's typed compile-time value representation. It can represent scalar values and supported aggregate/reference forms that analysis has proven constant.

A successful `comp` expression can collapse into `CheckedExpr::Const(value)`. Runtime codegen then emits/materializes the known value rather than re-executing the original source subtree.

## Compile-time evaluator boundary

`comp_eval.rs` evaluates an already semantically understood checked expression environment. It is not a second parser/type checker.
The stateful interpreter remains cohesive in `comp_eval.rs`; its focused unit tests live in `comp_eval/tests.rs` so production control flow is not buried under test fixtures.

When compile-time execution calls an Omega function, the evaluator obtains the checked function body through a resolver callback (`CompFunctionResolver`/driver implementation) rather than reaching into driver caches/filesystem directly.

Likewise, references to other compile-time declarations use already resolved `ConstValue`s when available.

This keeps compile-time execution inside the same item-query/semantic ownership model.

## Fuel

Compile-time evaluation has an implementation fuel budget to turn runaway loops/recursive calls into a diagnostic rather than hanging the compiler. The exact numeric limit is non-normative and tracked as an implementation limitation under [`../issues/compiler-limitations.md`](../issues/compiler-limitations.md).

Do not write language semantics that depend on “N evaluator steps”.

## Compile-time values and dead-code usage

A checked subtree replaced by a `ConstValue` is no longer visible to later checked-tree usage traversal. Therefore the analyzer records relevant field/variant usage during compile-time evaluation and returns it separately to the driver, which merges it into whole-program usage before dead-code warnings.

## Constant emission

Codegen owns conversion of `ConstValue` into native LLVM values/memory/data objects. Shared architecture rules include:

- scalar constants become ordinary LLVM constants;
- aggregate constants follow the same shared field/leaf/byte layout as runtime-built values;
- addressable byte blobs are emitted as anonymous data;
- repeated content-addressed const blobs may be deduplicated within a compilation unit;
- codegen does not re-run compile-time semantic evaluation.

## Representation changes checklist

Changing a type's runtime representation is cross-cutting. Audit, in order:

1. normative language/ABI promise if observable;
2. `ResolvedType` shape;
3. `layout.rs` leaves/size/alignment/offsets;
4. `ConstValue` if constants can contain the type;
5. shared ABI;
6. MIR place/expression shape only if needed;
7. codegen's leaf/place/constant handling;
8. mangling if the type's external identity changes;
9. separate-compilation tests.

Do not patch codegen's offset arithmetic as the primary implementation of a new layout rule.

## Representation couplings worth preserving

A few analyzer-side representation details are intentionally shared by layout, constant evaluation, and later lowering:

- Aggregate flattening includes real padding/filler leaves where required; byte offsets and flattened positional representation must stay consistent.
- Enum values retain full enum layout even when semantic analysis knows a specific variant. The representation is tag + header/shared fields + payload storage large/aligned enough for every variant body, which is what makes refinement-to-plain widening representation-preserving.
- Compile-time projected assignment has no backing memory. It rebuilds the containing `ConstValue` tree and writes the rebuilt root back to its binding.
- Dynamic-dispatch/vtable values are runtime constructs and have no compile-time `ConstValue` representation. If analysis proves a checked tree exhaustive but the interpreter later reaches "no matching arm," treat that as an analyzer/interpreter invariant violation rather than ordinary user input.

When these representations change, audit MIR/codegen and ABI documentation together instead of compensating with local comments.
