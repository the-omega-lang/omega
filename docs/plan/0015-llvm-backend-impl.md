# LLVM backend, and the backend seam it needs

## Task Description

- **What is being asked:** a second `omega-codegen` backend built on `inkwell`
  (LLVM 21), at capability parity with the Cranelift backend; a widened target
  set no longer bounded by Cranelift's ISA list, with a clear error when a
  target is asked of a backend that cannot serve it; every existing `omgc` flag
  working on both backends; and `core`/`std`/`examples` compiling and running
  correctly through LLVM.

- **Purpose:** Cranelift stays the fast development backend. LLVM buys target
  breadth — above all *freestanding* targets, which Omega cannot express at all
  today despite "embedded as a first-class target" being a stated goal — and
  the features Cranelift lacks (inline assembly, which this plan deliberately
  does not implement but must not obstruct).

- **Reasoning:** the honest version of "add a second backend" is mostly *not*
  the second backend. The Cranelift backend is already well-separated
  (`BackendKind` dispatch, `CodegenRequest`, and the backend-agnostic
  `layout`/`Leaf` split, where `cranelift/leaf.rs` is genuinely the only place
  a `Leaf` becomes a Cranelift type). What is *not* separated is a set of
  decisions the backend currently re-derives: the ABI, the symbol name, the
  linkage, and the alignment of every memory access. Each is a place where two
  backends can silently disagree, and disagreement means a `core.o` built with
  one backend mislinks against a `main.o` built with the other — which
  `justfile:3` does as a matter of course. Those decisions move to one shared
  home *first*; the LLVM backend is then written against a corrected seam
  rather than ported against the current one and patched afterwards.

  Alternatives rejected:
  - *Write the LLVM backend first, unify afterwards.* Rejected: it doubles the
    surface that has to be re-verified, and the alignment defect below is the
    kind that passes tests and fails in the field.
  - *Represent `Target` as an opaque triple string.* Rejected: layout math must
    answer `pointer_bytes` for every target. A string Omega has never heard of
    makes that a guess.
  - *Fix the C ABI as part of this work.* Rejected as scope; see "mirror now,
    fix later" below.

- **Resolved concerns:**
  1. *The analyzer is target-blind and hardcodes 64-bit pointers.*
     `comp_eval.rs:419` (`total_bytes(target, 8)`), `resolved_type.rs:834,839`
     (`ISize`/`USize` → 64), `resolved_type.rs:972,976` (range bounds),
     `resolved_type.rs:882` (`primitive_byte_size(Pointer)` → 8). All
     deliberate and documented, all correct only while pointer width is 8. The
     targets worth unlocking are mostly 32-bit. **Settled:** `Target` moves
     into `omega-analyzer` and is threaded through `Driver::compile`; the four
     hardcodes read the real width. Done first, before any LLVM code.
  2. *`Target` is a closed 2×3 enum with no bare-metal option.* **Settled:**
     widen it and add `Os::None`, keeping it structured rather than a string,
     and validate against the backend at target-machine construction.
  3. *The calling convention is x86_64-shaped and diverges from C.*
     **Settled:** mirror it exactly so both backends agree, centralize it so
     there is one definition, and record the divergence in the docs as debt
     rather than fixing it here.
  4. *The MIR omits decided facts the backend reconstructs.* **Settled:** ABI
     signature, symbol, linkage, and per-access alignment become carried facts.

## Technical Details

### What changes

**`omega-analyzer`**
- New `target.rs`, moved from `omega-codegen/src/target.rs`. `Target`, `Arch`,
  `Os`, `TargetParseError` keep their shape; `to_triple` does **not** come
  along — `target_lexicon` is a Cranelift concern, so each backend maps
  `Target` to its own triple type. This mirrors the existing `Leaf` →
  `cranelift::leaf::cranelift_type` precedent exactly: the analyzer owns the
  vocabulary, each backend owns its mapping.
- `Arch` gains `X86`, `Armv7`, `Thumbv7em`, `Riscv32`, `Riscv64`. `Os` gains
  `None` (freestanding). `pointer_bytes` becomes a real per-arch match;
  `pointer_bits` is added for the numeric-width call sites.
- `Target::parse`/`Display` extend to the new names; `none`/`freestanding`
  both parse to `Os::None`.
- `resolved_type.rs`: `numeric_kind`, the `ISize`/`USize` range bounds, and
  `primitive_byte_size` take the target's pointer width instead of assuming 64.
- `comp_eval.rs:419`: `total_bytes(target, target.pointer_bytes())`.
- `Analyzer`/`Driver` carry a `Target`; `Driver::compile` grows a target
  parameter.

**`omega-mir`**
- `MirFunctionDef` gains `symbol: String` and `linkage: MirLinkage`
  (`Export` | `Weak`), decided during lowering. `MirExternDeclaration` gains
  `symbol: String`.
- `lower_program` grows an `entry: &[Ident]` parameter, so the entry-point
  decision (`path == entry && name == "main"` → bare `main`) is made once,
  here, instead of by string comparison inside each backend.
- New dependency on `omega-mangle`; the whole
  `match (&f.mangling, &f.conformance_owner, &f.primitive_target)` dispatch
  currently in `cranelift/item.rs:77-113` moves here verbatim.
- Every `MirProjection`-derived access site carries the alignment of the access
  (see "Chosen approach").

**`omega-codegen`**
- New `abi.rs` at the crate root, beside `mangle.rs`: `AbiSignature { params:
  Vec<Leaf>, ret: AbiReturn }` with `AbiReturn::{Void, Direct(Vec<Leaf>),
  Indirect}`, built from `(Target, ResolvedFunctionType)`. Absorbs
  `cranelift/function.rs`'s `needs_sret` and parameter flattening, and
  `cranelift/expr.rs`'s `promote_variadic_arg` (a C ABI rule, not a Cranelift
  detail). Both backends consume it; neither re-derives it.
- `BackendKind` gains `supports(target) -> bool` and a diagnostic for the
  unsupported combination.
- `cranelift/` is rewired onto `abi.rs`, the MIR-carried symbol/linkage, and
  the MIR-carried alignment. Its observable output must not change.
- New `llvm/` module behind a new `llvm` Cargo feature, mirroring the
  Cranelift module's split: `mod.rs`, `leaf.rs`, `function.rs`, `item.rs`,
  `expr.rs`, `place.rs`, `vtable.rs`.

**`omgc`** — imports `Target` from `omega_analyzer`; `--backend=llvm` becomes
valid; `--help` lists both.

**Docs** — `docs/16-mir-and-codegen.md` (the seam and the new shared modules),
`docs/14-known-issues.md` (the ABI debt entry), `docs/10-modules-and-linkage.md`
(target syntax), `docs/09-annotations.md` if `sizeof` wording needs the
pointer-width correction.

### What must not change

- **Cranelift's emitted output.** Every refactor in Phase A/B is
  behaviour-preserving for the Cranelift backend; the symbol-count and
  execution gates must be identical before and after.
- **The MIR's CFG shape.** Blocks, terminators, the no-block-arguments
  decision, and the tree-shaped `MirExpr` all stay exactly as they are. This
  plan adds *facts* to the MIR, it does not restructure it. Three-address form
  remains out of scope (`docs/16`'s own caveat).
- **The calling convention itself.** Mirrored, not fixed.
- **Inline assembly.** Not implemented. The seam must simply not be made harder
  to add later.
- **`omega-mangle`'s algorithm.** Only the *dispatch* moves; the encoding is
  untouched.

### Chosen approach

*One decision, one home.* Each fact the backends need is computed exactly once,
upstream, and read downstream:

| Fact | Was | Becomes |
|---|---|---|
| sret vs registers | `cranelift/function.rs:30` | `abi.rs` |
| aggregate flattening | `cranelift/function.rs:62` | `abi.rs` |
| C variadic promotion | `cranelift/expr.rs:650` | `abi.rs` |
| symbol name | `cranelift/item.rs:77-113` | `MirFunctionDef::symbol` |
| entry-point `main` | `cranelift/item.rs:94` | MIR lowering |
| linkage | `cranelift/item.rs:36` | `MirFunctionDef::linkage` |
| access alignment | implicit in `MemFlags::new()` | carried on the access |

**Alignment is the one that must not be got wrong.** Omega packs aggregates by
default (`pack = 1, align = 1`, `docs/09`; `layout::type_alignment` returns 1
absent `@layout(align)`), so field accesses are in general unaligned.
Cranelift's `MemFlags::new()` sets no `aligned` flag, which reads as "not known
to be aligned" — conservative and correct. LLVM's default for `load`/`store` is
*natural* alignment, and violating it is UB the optimizer exploits. Carrying an
explicit alignment makes both backends state the same thing rather than each
assuming its own default.

**Unsupported targets fail once, in the same place.** `BackendKind::supports`
is consulted in `omega_codegen::generate` before any backend work begins, so
`--target=riscv32-none --backend=cranelift` produces one clear error naming both
the target and the backend, rather than a Cranelift ISA-lookup failure.

**The `todo!()`s become shared rejections.** Assignment into a parameter
(`place.rs:432`), a parameter's address, extern data declarations
(`function.rs:126`), and `MirPlaceRoot::Global` storage are today per-backend
`todo!()`s. With two backends, "unimplemented" must not be a backend property —
otherwise the language's accepted set depends on `--backend`. They move to one
shared check that rejects identically on both.

**A safe narrowing, to stop a silent miscompile.** Omega's convention is
internally consistent, so Omega-to-Omega calls are correct on every backend and
target. It is *not* the platform C ABI: a `struct { i32 a; i32 b; }` is one
eightbyte in `rdi` under SysV, but flattens to two parameters here. Today's C
interop is scalars and pointers only, so nothing is broken in practice. To keep
it that way, reject an aggregate passed or returned **by value across an
`extern` boundary** with a diagnostic pointing at the debt entry. This turns a
future silent miscompile into a compile error, costs nothing now, and is one
check to delete when the real C ABI lands.

### Risks and open questions

- **Mach-O symbol prefixing.** Mach-O prefixes symbols with `_`, and
  `cranelift-object` may apply this under the hood. If LLVM handles it
  differently, the two backends' symbols diverge on macOS. *Verify empirically
  during Step 12; do not assume either way.*
- **Widening the target set widens an x86_64-shaped ABI.** The new arches
  inherit a convention whose sret rule is justified by "rax/rdx". Omega-to-Omega
  is self-consistent, so this is sound within Omega; it is the C boundary that
  is wrong, and more wrong on non-x86_64. The narrowing above contains it. Flag
  rather than silently extend if a step seems to require per-arch ABI work.
- **`Os::None` has no linker story yet.** Producing a freestanding object is in
  scope; *linking* one needs a linker script and an entry convention that this
  plan does not define. Stop at "emits a correct `.o`" and flag anything beyond.
- **LLVM opt levels are finer than Cranelift's.** Cranelift collapses O1/O2;
  LLVM need not. Let each backend honour `-O<n>` natively and document the
  difference rather than inventing an artificial match.
- **inkwell 0.10 / LLVM 21.** Feature `llvm21-1`. The container provides LLVM
  21.1.2 with static archives, verified linking under
  `x86_64-unknown-linux-musl` with `crt-static`.

## Implementation Plan

Each step leaves the tree buildable and every existing gate passing.

### Phase A — make the seam target-aware

1. **Move `Target` into `omega-analyzer`.** Create
   `compiler/omega-analyzer/src/target.rs` from
   `compiler/omega-codegen/src/target.rs`, minus `to_triple`. Move `to_triple`
   into `cranelift/mod.rs` as a private `fn triple_for(target: Target) ->
   Triple`. Update `omega-codegen` and `omgc` to import from
   `omega_analyzer`. Do **not** re-export from `omega-codegen` — one home.
2. **Widen `Target`.** Add `Arch::{X86, Armv7, Thumbv7em, Riscv32, Riscv64}`
   and `Os::None`. Make `pointer_bytes` a real match, add `pointer_bits`,
   extend `parse`/`Display`/`TargetParseError` messages to list the new names.
3. **Thread `Target` through analysis.** Add it to `Driver`/`Analyzer`
   construction and `Driver::compile`; pass it from `omgc`. Replace the four
   hardcodes: `numeric_kind`'s `ISize`/`USize`, the `ISize`/`USize` range
   bounds, `primitive_byte_size`'s `Pointer` arm, and `comp_eval.rs:419`.
4. **Add the backend capability check.** `BackendKind::supports(Target)` plus
   `BackendKind::ALL`-driven wording; consult it at the top of
   `omega_codegen::generate`.

### Phase B — move decided facts into the MIR

5. **Symbol and linkage into the MIR.** Add `symbol`/`linkage` to
   `MirFunctionDef` and `symbol` to `MirExternDeclaration`; add `omega-mangle`
   to `omega-mir`'s dependencies; give `lower_program` its `entry` parameter;
   move the mangling dispatch and the `"main"` special case out of
   `cranelift/item.rs`. The Cranelift backend now reads `f.symbol`/`f.linkage`.
6. **Create `omega-codegen/src/abi.rs`.** `AbiSignature`/`AbiReturn`, built
   from `(Target, ResolvedFunctionType)`; absorb `needs_sret`, parameter
   flattening, and `promote_variadic_arg`. Rewire `cranelift/function.rs` and
   `cranelift/expr.rs` onto it.
7. **Carry alignment on accesses.** Compute each access's alignment once from
   `layout::type_alignment`/`stack_align_shift` and carry it to the point of
   load/store. Cranelift keeps emitting its conservative flags; the value is
   what LLVM will consume in Step 13.
8. **Unify the unimplemented cases.** Replace the four backend `todo!()`s with
   one shared rejection so both backends refuse the same programs.
9. **Reject aggregates by value across `extern` boundaries** with a diagnostic
   naming the C-ABI debt entry.

*Gate for Phase B: Cranelift output must be unchanged. Capture `nm
--defined-only` counts for `core.o`/`std.o` before Step 5 and diff after Step 9.*

### Phase C — the LLVM backend

10. **Dependencies and feature.** Add `inkwell` (feature `llvm21-1`) to
    `omega-codegen` behind a new `llvm` Cargo feature, mirroring how the
    `cranelift` feature is declared. Add `BackendKind::Llvm` behind it.
11. **Module skeleton.** `llvm/mod.rs`: context, module, target machine from
    `Target` (PIC reloc model, function sections — both required, or
    `--gc-sections` stops reclaiming and every `just` recipe regresses), and
    `finish()` covering `EmitKind::{Obj, Ir, Asm}`.
12. **Types and symbols.** `llvm/leaf.rs` mapping `Leaf` to LLVM types — the
    exact counterpart of `cranelift/leaf.rs`. `llvm/item.rs` declaring
    functions/globals from the MIR-carried symbol and linkage. *Verify Mach-O
    underscore handling here.*
13. **Functions, places, expressions.** `llvm/function.rs` building signatures
    from `abi.rs`; `llvm/place.rs` and `llvm/expr.rs` mirroring their Cranelift
    counterparts, emitting **explicit alignment** on every load and store from
    the Step 7 value.
14. **Vtables, globals, constants.** `llvm/vtable.rs` and the constant/blob
    dedup paths, mirroring `cranelift/vtable.rs` and `cranelift/expr.rs`'s
    `bytes`/`const_blobs` maps.

### Phase D — integration

15. **CLI.** `--backend=llvm` valid; `--help` lists both; the unsupported
    target/backend error reachable from a real command line.
16. **Gates.** Add LLVM counterparts of the existing `just` recipes and one
    **mixed-backend** recipe linking a Cranelift `core.o` against an LLVM
    `main.o`.
17. **Docs.** `docs/16` for the new seam; `docs/10` for the target syntax;
    **`docs/14`'s "Design debt worth watching"** for the ABI entry — stating
    that Omega's convention is internally consistent across backends and
    targets but is *not* the platform C ABI, that aggregate-by-value across
    `extern` is rejected until it is, and that `needs_sret`'s threshold is an
    x86_64 fact now applied to every arch.

## Testing

### New cases
- **Phase A:** `Target::parse`/`Display` round-trips for every new arch/OS,
  including `none`/`freestanding`. `pointer_bytes` per arch. A 32-bit target
  where `comp sizeof<usize>` evaluates to **4**, and where a `usize` literal
  above `u32::MAX` is rejected — both would silently pass before Step 3.
- **Phase B:** `AbiSignature` unit tests covering void, `Never`, one leaf, two
  leaves, three leaves (sret), and variadic promotion of `u8`/`i16`/`f32`/
  `bool`. Symbol/linkage assertions on lowered MIR, including the entry `main`.
- **Phase C:** every existing `just test-*` program, built and run through
  `--backend=llvm`, asserting the same exit codes and stdout as Cranelift.
- **Phase D:** the mixed-backend link runs and produces the same result.

### Negative cases
- `--target=riscv32-none --backend=cranelift` → one error naming both target
  and backend, not a raw ISA-lookup failure.
- `--target=sparc-linux` → unknown-arch error listing the supported set.
- An `extern` function taking a struct by value → the new diagnostic, naming
  the debt entry, not a silent miscompile.
- The four previously-`todo!()` constructs → identical rejection on both
  backends.

### Regression risk
- Highest: Phase B, because it rewires the Cranelift backend without intending
  to change it. `compiler/omega-driver/tests/` (`conform.rs` at 81 tests is the
  broadest) plus the full `just` suite — `test-io`, `test-stdio-contract`,
  `test-core-only`, `test-root-layout`, `test-allocator-only`,
  `test-multi-print`, `test-range`, `test-char`, `test-spec-dispatch`,
  `test-spec-calls`, `run-exec` (expects 69).
- The `nm --defined-only` symbol-count comparison for `core.o`/`std.o` is the
  sharpest detector of an accidental linkage or mangling change in Step 5.
- Second highest: alignment in Step 13. A wrong `align` passes every functional
  test at `-O0` and fails at `-O2`. Run the LLVM gates at `-O0` **and** `-O3`.

### Target coverage
- `x86_64-linux` on both backends — the full existing gate suite.
- `aarch64-linux` — object emission on both, verifying they agree.
- One 32-bit target (`riscv32-none` or `thumbv7em-none`) on LLVM only —
  emission plus the `sizeof<usize> == 4` assertion, which is the whole point of
  Phase A.
- A freestanding (`Os::None`) object emitted and inspected with `nm`. Linking a
  freestanding image is explicitly out of scope.
