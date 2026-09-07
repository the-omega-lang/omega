# Make explicit alignment an address guarantee

## Task Description

- **Deliverable:** `@layout(align = n)` establishes a minimum address alignment for compiler-created storage and propagates through inline containment. Value construction, constants, calls, and array strides obey that layout. Add `alignof<Type>` and alignment-aware standard allocation so heap-backed values and generic collections can satisfy the same contract.
- **Purpose:** close the alignment issue in `docs/issues/known-issues.md`, including nested `std::atomic` wrappers, without inserting checks or fallback locks into atomic operations.
- **Chosen direction:** retain packed primitive defaults; propagate explicit alignment through aggregate members. Keep `omega-analyzer::layout` the representation owner. Implement portable aligned allocation once in `std::alloc`, above the existing byte allocator gap.
- **Scope assumption:** this proposal includes heap allocation and collection integration. The narrower compiler-only alternative would leave a separate allocator limitation and would not justify closing the entire issue.
- **Rejected alternatives:** natural-aligning every primitive changes unrelated packed layouts; fixing only stack alignment leaves constants, arrays, and heap storage broken; weakening all LLVM accesses to alignment 1 neither fixes atomic addresses nor repairs missing padding; a fixed 16-byte allocator promise cannot support arbitrary explicit alignment. A new required platform gap is unnecessary when the existing byte allocator can back a portable adapter.

## Technical Details

### Initial context boundary and evidence

Start with `compiler/omega-analyzer/src/layout.rs`, `docs/language/annotations-and-sizeof.md`, the Alignment section of `docs/language/atomics.md`, and `docs/architecture/types-layout-and-const-eval.md`. Open `docs/architecture/abi-and-representation.md` for the value/call boundary. The affected downstream owner is `compiler/omega-codegen/src/llvm/`.

Verified defects and interfaces:

- `layout::type_alignment` currently returns declared struct/enum alignment and 1 for other shapes. `leaves_of` rounds struct/enum sizes using declared alignment; `union_bytes` does not round to member alignment. `layout_fields` already places fields using `type_alignment` and records byte offsets, leaf starts, and padding.
- `llvm/expr.rs`'s `MirExpr::StructLiteral` and `llvm/constant.rs`'s `emit_const_value(ConstValue::Struct)` concatenate field leaves without layout padding. The constant-blob writer already uses shared field offsets, so the two representations disagree.
- `llvm/constant.rs::declare_blob` hardcodes alignment 1. Its callers cache weak constant globals using hashes that omit the containing type's layout; increasing alignment only on a cache hit in one module would not establish a separate-compilation contract.
- `llvm/expr.rs` and `llvm/constant.rs` allocate union construction scratch at alignment 16. `llvm/place.rs::place_storage_address` also spills SSA values at alignment 16 without receiving their semantic type/alignment.
- Other storage paths already ask shared layout for alignment: `llvm/function.rs` locals/parameter homes, `llvm/item.rs` globals/declarations, expression-root and aggregate spills in `llvm/place.rs`, and direct/dynamic hidden-result slots in `llvm/expr.rs`. Verify these consumers after changing layout.
- `compiler/omega-mir/src/lower/place.rs::place_align` initializes `MirPlace::align` from the projected type. `llvm/place.rs::{load_scalars,store_scalars,offset_align}` emits conservative alignment claims using offsets. `compiler/omega-codegen/src/abi.rs::{AbiSignature::build,AbiReturn::for_type}` consumes the shared leaf sequence.
- `runtime/core/platform.omg::GlobalAllocator` provides only byte-count allocation, free, and reallocation. `runtime/std/alloc.omg` wraps it. Typed allocation happens in `runtime/std/list.omg`, `linked_list.omg`, and `hash_map.omg`; `String` and `HashSet` use those collections. Linux's implementation in `runtime/plat/os/linux/common/allocator.omg` currently returns a fixed 16-byte-aligned address.

### Settled layout and pointer contracts

Let `A(T)` be a type's effective alignment:

| Shape | Effective alignment |
| --- | --- |
| Struct | Maximum of declared alignment and every field's `A` |
| Named enum | Maximum of declared alignment and tag, header, shared dynamic, and all variant-body fields' `A` |
| Anonymous enum | Maximum of its tag and every member's `A` |
| Union | Maximum of every member's `A`, defaulting to 1 |
| Fixed array `[N]T` | `A(T)`, including when `N` is zero |
| Primitives, pointers, functions, slices, strings, spec-object handles | Existing alignment 1; do not follow their pointees |

An explicit smaller `align`, including 1, never lowers a contained member's requirement. `pack` retains its current chunk-placement meaning and cannot weaken alignment. Unions inherit alignment without acquiring `@layout` syntax.

Keep the power-of-two/u32 annotation rule, and diagnose a requested alignment that cannot be represented by the target's `usize` before emission. This matters on 16-bit targets: the query must never truncate an effective alignment. Place this check with annotation validation in `compiler/omega-analyzer/src/annotations.rs::resolve_layout`; describe the target constraint in the language chapter. Do not allocate enormous leaf vectors merely to test this rejection.

Round struct, enum, and union storage size to effective alignment. Fixed-array stride is the resulting `sizeof<T>`; each element and nested member must satisfy its own alignment. Union members remain at offset zero, and enum refinements keep the parent's complete layout. Empty types may retain size zero while requiring an aligned address; do not manufacture semantic bytes for them. Recursion follows only inline containment and queries completed semantic types, never pointer targets or unfinished nominal cells.

All compiler-created addresses of `T` must satisfy `A(T)`: locals, globals, parameter homes, temporaries, hidden results, constant references, and slice backing data. Raw pointer casts remain unchecked address reinterpretations. Before typed access, callers must supply enough valid storage aligned for the accessed type; casts do not realign memory. External data definitions must satisfy the declared Omega layout. Merely storing a pointer does not require its pointee's alignment. Raw atomic width-gap calls independently require the operation's natural alignment, even though primitive `A` stays 1.

The flattened runtime/constant value must have exactly the leaf types, count, and order returned by shared layout, including interior and trailing padding. Construct padding as zero filler for deterministic emission, without promising padding-byte values as language semantics. Preserve source evaluation order and evaluate every field initializer once before placing its leaves.

### Alignment query and allocation API

Add contextual `alignof<Type>` alongside `sizeof<Type>`. It returns `usize`, evaluates from shared semantic layout at compile time or runtime, and works after generic substitution, including aliases and compile-time expressions. It reports Omega alignment, so `alignof<u64>` remains 1. Give storage-less `void`/`never` alignment 1 consistently with the existing helper; reject non-value spec definitions through the normal type diagnostic. Keep annotation argument syntax unchanged: this work does not add `alignof` inside `@layout`.

Make these `runtime/std/alloc.omg` functions exposed:

```omega
alloc(size: usize, align: usize) => *mut u8
realloc(ptr: *u8, size: usize, align: usize) => *mut u8
free(ptr: *u8) => void
```

These are the single std allocation family; change its current internal callers together. `GlobalAllocator` stays a raw byte-allocation capability with its existing signatures. Its results must be paired with its own free; std adapter results must be paired with std free. No compiler-generated heap allocation, new required core capability, or libc dependency is introduced.

Implement the adapter with checked over-allocation and address rounding. Reserve a private header immediately before the returned address containing the original raw allocation pointer and requested byte count. Allocate `header_bytes + (align - 1) + max(size, 1)`, rejecting overflow before calling the gap. Check address-rounding arithmetic as well. Header fields use ordinary packed Omega access; their addresses must not be assigned invented natural-alignment guarantees.

- Alignment must be a nonzero power of two representable by `usize`. Invalid alignment, overflow, or raw allocation failure returns null.
- A successful zero-size allocation still returns an aligned, freeable address; its logical requested size remains zero.
- `free(null)` is a no-op; other frees recover the raw pointer from the header.
- `realloc(null, size, align)` behaves as allocation. Otherwise allocate the replacement, copy `min(old_requested_size, size)` bytes, then free the old block. Failure leaves the old block untouched. A successful request may change alignment. Do not pass the adjusted pointer to the raw gap's `realloc`.
- Document header/slack overhead and allocate-copy-free reallocation. Moving storage that is being accessed concurrently remains the caller's error; alignment does not make relocating a live atomic safe.

Collections pass alignment of the actual allocated type: `T` for `List<T>`, `Node<T>` for `LinkedList<T>`, and `Entry<K,V>` for hash entries. Bucket arrays use the bucket-element type's alignment. List growth must retain the requested alignment. Existing explicit ownership/free APIs remain intact.

### Compatibility and boundaries

Sizes, field offsets, and flattened Omega call signatures can change for aggregates containing aligned members. Rebuild all dependent objects and runtime packages; do not attempt mixed old/new object compatibility. Preserve the existing scalar/pointer foreign-convention boundary and do not add C aggregate ABI classification.

Out of scope: atomic instruction/order changes, naturally aligning unannotated primitives, checked-pointer types, new union annotations, general allocator performance or collection failure-policy redesign, and unrelated constant hashing or compiler layout-overflow cleanup. Constant identity changes are limited to making differently laid-out/aligned typed materializations distinct.

Escalate if an accepted alignment cannot be represented by a supported target's storage/object machinery: do not silently cap it, drop LLVM alignment, or promise success. Likewise, do not resolve a newly exposed recursive-value-layout problem by following nominal pointers or introducing stale caches.

## Implementation Plan

1. **Fix the semantic layout owner.** In `compiler/omega-analyzer/src/layout.rs`, implement the transitive rules, effective size rounding, and union rounding. Add a shared struct-layout result/helper, using `FieldLayout`, that includes trailing padding as well as field starts and offsets; route struct flattening and construction consumers through it. Keep named/anonymous enums on `EnumView`. Add focused tests in `compiler/omega-analyzer/src/layout/tests.rs` before changing emission.

2. **Make aggregate construction match shared layout.** In `compiler/omega-codegen/src/llvm/expr.rs` and `constant.rs`, use one common struct-leaf assembly path for evaluated field values and constants. Populate a complete padding-bearing leaf vector from shared field starts. Check runtime arrays, nested structs, enum/union constructors, constant arrays, and whole-value assignment/copy against the same invariant. Initialize any enum scratch gaps/trailing bytes that the complete leaf load will read, as already done for opaque payload regions.

3. **Carry effective alignment to every storage source.** Replace fixed union/value-address scratch alignment with type-derived requirements. Extend `place_storage_address` and its callers to carry the type or explicit proven alignment. Pass alignment into constant-global declaration for references and slice element storage. Include deterministic type/layout identity, alignment, and materialization shape/count in the constant cache/symbol input so incompatible weak definitions cannot collide; reuse `omega_analyzer::type_key::structural_key` and the existing data-symbol mechanism, without changing the mangled function/type grammar. Cover typed constants containing pointer relocations too. Verify globals, local-frame offsets/base alignment, parameter homes, all spills, and hidden return destinations. Keep scalar alignment claims bounded by the proven accessed address; retain conservative offset weakening and make alignment-setting failures visible.

4. **Add the query through the existing `sizeof` route.** Start at `compiler/omega-parser/src/parser/expression.rs` and `ast/expression.rs`, then `compiler/omega-hir/src/{hir.rs,lower/expression.rs}`, `compiler/omega-analyzer/src/{analysis/exprs/mod.rs,checked.rs,comp_eval.rs}`, `compiler/omega-mir/src/{body.rs,lower/function/expr.rs}`, and `compiler/omega-codegen/src/llvm/expr.rs`. Mirror the contextual parse and type-resolution flow with a new alignment-query variant; update exhaustive visitors, including parser macro expansion and MIR defer traversal, by targeted `Sizeof` reference search. Both evaluation routes call `layout::type_alignment`; neither LLVM's natural layout nor host width supplies the answer.

5. **Integrate explicit aligned allocation in one runtime batch.** Implement the adapter in `runtime/std/alloc.omg`, then change typed allocation calls in `list.omg`, `linked_list.omg`, and `hash_map.omg`. Preserve raw `GlobalAllocator` and the Linux/Windows/libc glue signatures. Correct the Linux allocator comment claiming its fixed alignment covers every Omega value. Test the adapter using both real hosted glue and a controlled custom byte allocator returning deliberately offset blocks; the implementation must not depend on the host heap's accidental alignment.

6. **Update owning documentation and conformance fixtures.** Clarify effective alignment, trailing rounding, and `alignof` in `docs/language/annotations-and-sizeof.md` and `types-and-primitives.md`; update the contradictory “not by inheritance” wording in `atomics.md`. State typed-access obligations beside pointer casts in `strings-casts-arrays-and-slices.md` and external-data obligations in `foreign-function-interface.md`. Add the query to `compile-time-evaluation.md` and `docs/guide/quick-reference.md`. Describe the adapter and pairing/cost contract in `docs/guide/standard-library.md`, with raw allocator boundaries in `platform-glue.md`. Update layout/value/storage contracts in the two relevant architecture documents. Remove the resolved alignment issue only after all phases pass; update `docs/issues/design-debt.md`'s obsolete declared-only-alignment description while retaining its independent packed-default concern.

## Testing

- **Layout component tests:** explicit-alignment propagation through every inline shape, weaker explicit container annotations, pack interaction, size rounding/array stride, zero-length arrays and aligned empty values, union maximum-size rounding, named/anonymous enum prefix and payload alignment, refinement invariance, and pointer recursion stopping. Exercise pointer-byte widths 2, 4, and 8. Verify unchanged packed primitive-only layouts.
- **Query component tests:** parser contextual-identifier behavior and malformed type syntax; analyzer/const-eval results for aliases, generics, arrays, composites, and pointer-width-dependent annotations. Test rejection of target-unrepresentable annotation alignment, as well as existing zero/non-power-of-two rejection. Use existing parser tests and the driver-test conventions in `compiler/omega-driver/tests/comp_generics.rs`; diagnostics must assert the intended reason.
- **Codegen tests:** add a dedicated new `compiler/omega-codegen/tests/alignment.rs`, following the real driver-to-IR harness in `tests/emission.rs`. Check alloca/global alignment above 16 (for example 64), typed constant references and slices, weak constant identity across source units, padding-bearing argument/result leaves, and safe load/store claims. Include x86-64 and AArch64, plus 16/32-bit emission using the target harness in `tests/target.rs` where supported.
- **Language conformance:** add a new root package `tests/layout_alignment/` with exact labeled `expected.stdout`. Assert address modulo alignment and actual field values after runtime and `comp` construction, mutation, copies, reordered initializer side effects, parameters, direct/indirect/dynamic calls and returns, pointer/slice indexing, and constant materialization. Cover nested structs, arrays with at least two elements and trailing members, unions, and named/anonymous enums. Use alignments above 16 to expose fixed-scratch defects. Specification trace: outward alignment and `sizeof` rules in `annotations-and-sizeof.md`, inline storage rules in `types-and-primitives.md`, and constant address materialization in `compile-time-evaluation.md`.
- **Atomic conformance:** extend `tests/t35_atomics/` with a leading byte, a nested wrapper container, a trailing byte, and a multi-element array; check alignment before load/store/RMW and assert resulting values. Include global/local and heap-backed forms. Give the existing raw-gap test slots explicit aligned owning storage so they meet `atomics.md` without relying on local-frame luck. No misaligned atomic execution as a negative test: that program has no specified result.
- **Allocation conformance:** add new `tests/aligned_allocation/` for powers of two through an alignment larger than a page, zero-size success, free/null handling, growth/shrink copies, alignment changes, and `List` growth plus node/hash-entry alignment. Add a component/workflow fixture with custom raw glue for deterministic misalignment, allocation failure, invalid alignment, and overflow. Verify raw-pointer recovery and that failed realloc leaves the old bytes/allocation live. No diagnostics are expected for runtime allocator failure; null is the API result.
- **Separate compilation and optimization:** extend the workflow pattern in `compiler/omgc/tests/separate_linkage.rs` with independently compiled Omega producer/consumer packages sharing aligned arguments/results, external globals, and weak constant references. Run the focused positive alignment cases at `-O0`, `-O2`, and `-O3`, rebuilding participating objects consistently. The ordinary root runner currently has no optimization selector; use a focused workflow harness with explicit CLI flags rather than inventing a runner flag. LLVM verification alone is insufficient; compare executable results.
- **Commands:** begin with `cargo test -p omega-analyzer layout`, then relevant parser/driver tests and `cargo test -p omega-codegen`. After `just build-omgc build-runtime`, run `./bin/test-runner layout_alignment aligned_allocation t15_annotations_and_sizeof t06_structs_and_unions t07_enums_and_pattern_matching t08_strings_casts_arrays_and_slices t27_anonymous_enums t34_comp_generics t35_atomics`. Finish with `cargo test -p omgc --test separate_linkage`, `just test-all`, and `just check-platform` because ABI, runtime allocation, and atomics cross package/platform boundaries. Add nested/heap alignment assertions to the existing `checks/smoke/` workflow where appropriate. Execute AArch64 regressions when a native/emulated runner exists; otherwise record execution as unavailable and report object/IR coverage separately. Confirm core-only/freestanding code still needs no allocator glue merely to use aligned declarations or `alignof`.
