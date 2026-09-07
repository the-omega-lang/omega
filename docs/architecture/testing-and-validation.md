# Architectural testing and validation

Omega uses several test layers. Choose the narrowest layer that proves the contract being changed, and expand only when correctness crosses a wider boundary.

## Test layers

### Implementation-detail Rust tests

Focused white-box tests may live beside the implementation (for example `src/<module>/tests.rs`) when correctness depends on private state, a local invariant, or an algorithm that is awkward to exercise through the crate API.

Use these sparingly for things such as:

- parser cursor/grammar helpers;
- type/layout/ABI algorithms;
- MIR construction invariants;
- mangling internals;
- backend-local lowering helpers.

These tests answer **how the implementation works**, not whether Omega as a language conforms to its specification.

### Component integration tests

Cargo integration tests under `<crate>/tests/` exercise a compiler component through its intended public/component boundary. Existing examples include parser, HIR, driver, and mangling coverage.

Use this layer for behavior that can be proven without compiling and executing a complete Omega program, such as:

- source -> parser/HIR behavior;
- semantic analysis and diagnostics;
- module/package discovery;
- MIR shape;
- symbol encode/decode round trips;
- codegen planning or backend-local contracts that do not require a linked program.

A compiler-error test must assert the intended diagnostic/reason, not merely that some error occurred.

### Language conformance / end-to-end tests

The root `tests/` directory is the executable language-conformance suite. Each **direct child directory** is one test package:

```text
tests/
  hello_world/
    hello_world.omg
    expected.stdout
    expected.stderr
    expected.status
```

`bin/test-runner` discovers those directories and, for each selected case:

1. invokes `bin/omgc-debug` on the test package for the host target, registering the current `core`, `std`, and `plat` source packages as externs;
2. if compilation succeeds, compiles any `*.c` sources the case ships (freestanding, no C runtime), gathers the case's per-source objects in sorted path order, and links them with the prebuilt runtime objects;
3. executes the resulting program;
4. compares any present `expected.stdout` and `expected.stderr` files byte-for-byte with the relevant captured output, and any present `expected.status` file with the program's termination status.

The link is `-nostdlib -static -no-pie -Wl,--gc-sections`: the platform package supplies the ELF entry symbol itself, so no CRT startup object or libc may take part. A case whose subject is the C ABI ships its own freestanding C helper rather than pulling a C runtime into every conformance executable.

The runner keeps captured output in memory. Per-test artifacts live under `<artifacts>/tests/<case>/`: the case's object tree in `objects/`, mirroring its source layout, its native helper objects, plus the linked executable. `<artifacts>` defaults to `target/` and can be overridden with `OMEGA_ARTIFACTS_DIR`; the runtime packages' object directories (`<artifacts>/<target>/core/`, `std/`, `plat/`) are read from the same root. Each case's output directory is cleared before compiling, so a removed source cannot leave a stale object in the link.

These cases are **language conformance tests implemented end-to-end**. They should be derived from observable rules in `docs/language/`. A compiler bug must not be encoded as the expected language behavior merely because the current implementation happens to do it.

Use this layer when the claim is about accepted/rejected Omega source or observable execution semantics, especially when correctness depends on multiple compiler stages, native code generation, linking, or runtime/library behavior.

## Expected-output conventions

Expectation files are optional and exact:

- `expected.stdout` checks stdout;
- `expected.stderr` checks stderr;
- `expected.status` checks the termination status of a successfully linked program.

If compilation fails, the current runner compares the compiler's stdout/stderr against the same expectation files. A compile failure without `expected.stderr` is treated as unexpected. If compilation succeeds, link failure is always a test failure.

Only a *failed* compilation exposes compiler output to these files; once compilation succeeds the runner compares the linked program's own streams, so warnings emitted by a successful compile are invisible here. Assert warning behavior in the owning crate's tests (`compiler/omega-driver/tests/` for whole-package warnings) instead.

Without `expected.status`, a successfully linked program must exit successfully in addition to matching any expected streams. `expected.status` replaces that requirement with an exact decimal comparison, so a case may assert a deliberately abnormal termination -- a panic reaching the hosted handler, for example, which exits `134`.

Because the same files can describe compiler output for a negative test or program output for a successful test, keep each case intentionally single-purpose. If future test needs make that convention ambiguous, extend the runner deliberately rather than inferring intent from filenames or compiler behavior.

Give a `println$` call a short kebab-case label as its first argument (for example `println$("range-sum: ", total, " ", stop_at);`) whenever its output is not already self-identifying, so a mismatched `expected.stdout` line can be traced straight back to the call that produced it. Skip the label only when the call's sole argument is already fixed, descriptive text (for example `println$("newline literal matches");`); a label there would just repeat what the line already says.

## Running the suite

### Normal top-level gate

```text
just test-all
```

This is the normal repository entry point. The `justfile` builds the compiler/runtime artifacts required by the conformance runner, then invokes `bin/test-runner`.

### Focused conformance cases

When `omgc` and the runtime objects are already built:

```text
./bin/test-runner hello_world
./bin/test-runner case_a case_b
```

With no names, the runner executes every direct test package under `tests/`.

If runtime objects live outside the default `target/` directory:

```text
OMEGA_ARTIFACTS_DIR=/path/to/artifacts ./bin/test-runner hello_world
```

Direct runner invocation is intentionally a **test-only** operation: it does not build `omgc`, `core`, `std`, or `plat` for you.

## Language-specification coverage

`docs/language/` is normative. The root conformance suite is executable evidence that the implementation follows it.

When adding or changing a language rule:

1. identify the relevant specification statement;
2. add/update a positive case when the construct is valid;
3. add/update a negative case when rejection is part of the rule;
4. execute the program when runtime semantics are part of the rule;
5. prefer focused cases that prove one semantic claim clearly over large demo programs that happen to exercise many features.

A component test may localize a bug, but it is not a substitute for an end-to-end conformance case when the user-visible language behavior changed.

## Codegen validation

Shared language semantics, ABI, layout, symbols, and linkage are decided upstream of LLVM emission. When a change affects one of those shared contracts, verify the relevant conformance case through the real compile/link/run path rather than trusting a Rust unit test alone.

An LLVM-local instruction-selection/emission bug normally needs focused coverage for that emission path plus the shared MIR contract.

## Separate-compilation and linking validation

Package identity, mangling, ABI, and weak-linkage bugs can require more than a single package invocation. Use explicit multi-package/separate-process cases when the contract under test depends on:

- declared package/module identity;
- extern references;
- generic-instantiation ownership;
- duplicate weak folding;
- concrete strong-definition uniqueness.

The root language runner is intentionally simple; workflows that need several independently compiled packages, custom linker assertions, `nm`/`readelf`, or deliberately different runtime subsets may justify a focused compiler/workflow test or a small dedicated recipe rather than complicating every language case.

### Platform-matrix validation

`bin/check-platform` (`just check-platform`) is that dedicated recipe for the platform layer, driving the fixtures under `checks/`. It covers what one uniform link on one target cannot:

- every target root under `runtime/plat/target/` compiles for its own target, and its composition symlinks are intact;
- the hosted x86-64 Linux program links with no CRT and no libc, has no unresolved symbol and no dynamic dependency, and really reaches stdout, stdin, stderr, the heap and an aligned atomic;
- eight host threads agree on the final value of every atomic width, linked against `core` plus only the platform's single `arch/atomic` object -- the harness may use pthreads, since what must be free of a C runtime is the platform implementation it calls;
- each architecture emits the instructions its implementation claims (x86-64 locked forms, AArch64 baseline exclusives rather than optional LSE, AVR interrupt masking);
- the Windows objects import Kernel32 and nothing else and define this platform's own entry symbol rather than a CRT startup one;
- `avr-none` leaves the allocator and console gaps genuinely unresolved instead of stubbed, while filling all four atomic widths and the panic handler.

Cross targets are taken as far as the host toolchain honestly allows: `aarch64-linux` is linked into a static ELF, and the Windows link command is documented rather than faked because no import library is present. A check that cannot run reports as skipped with the reason, never as a pass.

## Runtime capability validation

When a change concerns `core`, `std`, platform glue, freestanding behavior, or section garbage collection, link/run coverage should reflect the actual capability boundary being claimed. Useful assertions include:

- core-only code does not require std/platform objects;
- allocator-using code does not retain unrelated console paths;
- console code requires the appropriate platform glue;
- unreachable capability references disappear under section garbage collection.

Link success/failure itself may be the primary assertion; symbol inspection can provide secondary structural evidence.

## LLVM-native verification

LLVM modules are explicitly verified before output.

This proves that emitted IR is structurally valid. It does **not** prove language semantics, ABI intent, or runtime behavior, so it does not replace conformance tests.

## Documentation consistency

When architecture or semantics change:

1. update the owning deep architecture document when implementation ownership/contracts changed;
2. update root `ARCHITECTURE.md` only if routing/ownership/pipeline changed;
3. update `AGENTS.md` / `CLAUDE.md` only if routine agent workflow or authority rules changed;
4. update `docs/language/` when observable language semantics changed;
5. add/update the corresponding root conformance case when a documented language rule changed;
6. add/remove `docs/issues/` entries for unresolved/resolved deviations.

Do not duplicate the same invariant across every layer.

## Verification selection

Prefer an expanding sequence:

```text
focused implementation/component test
    -> owning crate integration tests
    -> focused root language-conformance case
    -> full `just test-all` gate
    -> separate-compilation/runtime matrix only when the changed contract requires it
```

The goal is not to run the maximum number of commands. It is to use the smallest validation set that actually crosses every changed boundary.
