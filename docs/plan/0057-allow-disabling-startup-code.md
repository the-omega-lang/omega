# Opt-out platform startup code

## Task Description

- **Deliverable:** every startup declaration in `runtime/plat/` is gated on one
  standardized compiler definition, `def::omega_no_startup`. A build that
  supplies nothing keeps today's behavior exactly; a build that passes
  `-Domega_no_startup` to the `plat` compilation gets a platform package that
  declares no `_start`, no `omg_start`, no `main`, and no `_omg_main`
  reference, while keeping every other capability that platform fills.
- **Purpose:** a platform package currently forces a process entry point on
  every consumer. A shared library, a component linked into a foreign host
  program, or any image that already owns its entry point must be able to use
  `plat`'s allocator, console, panic and atomic glue without a colliding or
  dead startup symbol. Startup stops being mandatory without becoming a second
  mechanism: it is an ordinary `@cond` selection, which is what conditional
  compilation was added for.
- **Chosen direction:** per-item `@cond(not(def::omega_no_startup))` on the
  startup declarations, written in the platform sources themselves. No
  compiler change, no new CLI flag, no new builtin, no new composed target
  root. Polarity is negative because the language makes it so: an unsupplied
  definition reads as `false` in a boolean-expected position, so only a
  definition meaning *disable* can give a default-enabled switch.
  `omega_` is adopted as the reserved prefix for definitions the shipped
  runtime packages read, because definitions are one flat namespace shared by
  every package of a compilation (core, std, plat and the application all see
  the same `-D` set).
- **Rejected alternatives:**
  - *Positive polarity* (`@cond(def::omega_startup)`, disabled with
    `-Domega_startup=false`). Impossible as a default-on switch: absent means
    false in boolean position, and `equals(def::omega_startup, true)` is a
    value position, where an unsupplied definition is an error, not `false`.
  - *A `--no-startup` CLI flag or a new `@cond` builtin.* Duplicates the
    definition mechanism and gives the compiler knowledge of a runtime
    package's internals. `-D` already expresses this exactly.
  - *Filesystem composition* (a second target root per target, or omitting
    `plat/os/entry.o` from the link line). Doubles the committed symlink
    matrix, still emits the object, and cannot express
    `runtime/plat/libc/libc.omg`, where the startup adapter sits in the same
    file as the allocator, console and panic glue. Item granularity is the
    only granularity that covers all four sites.

## Technical Details

- **Initial context boundary:** `runtime/plat/`,
  [`docs/language/annotations-and-sizeof.md`](docs/language/annotations-and-sizeof.md#condcondition)
  for `@cond` rules, [`docs/guide/platform-glue.md`](docs/guide/platform-glue.md),
  [`docs/architecture/runtime-and-platform.md`](docs/architecture/runtime-and-platform.md),
  and `bin/check-platform`. No compiler crate is in scope.
- **Affected files/symbols** — the four startup sites, and nothing else in the
  tree defines an entry symbol:

  | File | Items to gate |
  |---|---|
  | `runtime/plat/os/linux/arch/x86_64/entry.omg` | `process_entry` (`@symbol(name = "_start")`) |
  | `runtime/plat/os/linux/arch/aarch64/entry.omg` | `process_entry` (`@symbol(name = "_start")`) |
  | `runtime/plat/os/windows/entry.omg` | `import root::os::api;`, `foreign _omg_main : () => void;`, `process_entry` (`@symbol(name = "omg_start")`) |
  | `runtime/plat/libc/libc.omg` | `foreign _omg_main : () => void;`, `platform_main` (`@symbol(name = "main")`) — and only those two; the `malloc`/`write`/`abort` declarations and every `glue` block stay unconditional |

  `runtime/plat/target/avr-none/` composes no entry point and needs no change.
- **Interfaces/invariants:**
  - `@cond` is accepted on `import`, `foreign-binding`, and
    `foreign-function-item` (`docs/language/grammar.md`, `item`), and coexists
    with `@naked`/`@symbol`; write `@cond` first, above the other annotations.
  - Every item in a file that becomes unreachable when startup is off must be
    gated with it, not left behind. On Windows that is why the `import` and
    the `foreign _omg_main` declaration are in the table: leaving the import
    would produce an `unused_import` warning, and leaving the `foreign`
    declaration would keep a dangling contract for a symbol nothing calls.
  - One definition name, spelled identically at all four sites. A platform
    that later grows a startup sequence gates it on the same name.
  - Default behavior is unchanged: `just build-runtime-for`, `bin/test-runner`
    and `just playground` pass no `-D`, so every existing conformance case,
    including `tests/t22_program_entry_point`, links against a `_start` exactly
    as it does today.
  - A module whose every item is filtered out is fine and needs no special
    handling: verified by compiling a two-module package with one module's
    sole item gated off, which emits an ordinary object defining no symbols.
    So with startup disabled, each `entry.omg` still produces
    `target/<target>/plat/os/entry.o`, empty. The check in step 6 must
    therefore assert *no entry symbol is defined*, not that no object exists.
  - The definition must be given to the `plat` compilation. Nothing in an
    emitted artifact records the configuration that produced it, so a build
    that disables startup and then links a `plat` built without the definition
    gets the entry point back; that is the ordinary `-D` contract and belongs
    in the guide, not in a compiler check.
- **Out of scope:**
  - The compiler's emission of `_omg_main` for a root `main`. That is the
    application package's own declaration; a library package simply has no
    `main`. Do not add a definition that suppresses it.
  - Making `omgc` produce shared objects, PIC codegen, or link lines for them.
  - Any compiler-side reservation or validation of the `omega_` prefix. The
    prefix is a documented convention; enforcing it would put runtime-package
    knowledge in the compiler.
  - Repairing anything else discovered in `runtime/plat/libc/libc.omg`; it is
    outside the normal build path and this task only gates its two startup
    items.
- **Risks/open questions:**
  - `runtime/plat/libc` is built only by `just build-plat-libc` and is
    currently unverified. If its baseline build already fails before any edit,
    report that rather than fixing it here.

## Implementation Plan

1. Gate the two Linux entry points. Add `@cond(not(def::omega_no_startup))`
   as the first annotation on `process_entry` in
   `runtime/plat/os/linux/arch/x86_64/entry.omg` and
   `runtime/plat/os/linux/arch/aarch64/entry.omg`. Extend each file's existing
   header comment by one sentence stating that the entry point is the one
   platform facility a build can decline, and naming the definition — this is
   local reasoning that belongs beside the declaration; the contract itself
   goes in the guide, not repeated here.
2. Gate the Windows entry point: the same annotation on all three items of
   `runtime/plat/os/windows/entry.omg`, with a one-line note on the `import`
   and the `foreign` declaration explaining that they exist only for the entry
   point and therefore share its condition.
3. Gate the libc adapter: the same annotation on `foreign _omg_main` and
   `platform_main` in `runtime/plat/libc/libc.omg`, leaving the surrounding
   allocator/console/panic items untouched.
4. Verify the default build is byte-for-byte unchanged in behavior: run
   `just build-runtime-all`, then `just test-all`. No conformance case, no
   `expected.*` file, and no `justfile` recipe changes in this task.
5. Verify the disabled build by hand once per shape before automating it:
   compile `plat:runtime/plat/target/x86_64-linux/`,
   `plat:runtime/plat/target/x86_64-windows/` and `plat:runtime/plat/libc/`
   with `-Domega_no_startup` into a scratch directory and confirm with `nm`
   that `_start`, `omg_start`, `main` and `_omg_main` are absent while
   `GlobalAllocator`, console and `PanicHandler` glue remain. Expect each
   `entry.omg` to still emit a symbol-free `plat/os/entry.o`.
6. Automate that in `bin/check-platform`:
   - add a `defines:` keyword argument to the `compile` helper (near line 74)
     that appends `-D` options, defaulting to none so every existing call is
     unchanged;
   - in the per-target section, add a check that compiles that target's plat
     root with `-Domega_no_startup` into `WORK` and asserts no object defines
     `_start`, `omg_start` or `main`, and that the allocator/console/panic glue
     symbols the target does fill are still defined — the point is that only
     startup disappears;
   - add the positive assertion for the Linux targets to match the Windows
     "the entry point is this platform's own" check, so the default build is
     asserted to define `_start`;
   - add one `runtime/plat/libc` check pair on `HOST` covering `main` present
     by default and absent with the definition.
7. Documentation, at the layer that owns each fact:
   - `docs/guide/platform-glue.md`: a short subsection under the capability
     matrix (the "process entry point" row) stating that startup is the one
     platform facility a build can decline, the exact definition name, a
     `omgc plat:... -Domega_no_startup` example, the consequence that the link
     must then supply its own entry point, and the requirement that the
     definition go to the `plat` compilation;
   - `docs/architecture/runtime-and-platform.md`: extend the entry/startup
     discussion to record that startup is selected by configuration rather
     than by a gap, and why — a gap needs exactly one glue, while startup must
     be able to be absent entirely, which is what `@cond` expresses;
   - `docs/guide/compiler-cli.md`, "Compiler definitions": one paragraph
     stating that definitions are a single flat namespace shared by every
     package of an invocation and that `omega_`-prefixed names are reserved
     for definitions the shipped runtime packages read, with
     `omega_no_startup` as the first of them;
   - `docs/language/` is untouched: `@cond` semantics do not change.

## Testing

- **New/changed cases:** none in root `tests/`. This changes no observable
  Omega language behavior — `@cond`'s selection semantics are unchanged and
  already covered by `tests/t45*`, including `@cond(false)` on an `import` in
  `tests/t45c_conditional_pruning`. The new behavior is a runtime-package
  configuration, and `bin/test-runner` links prebuilt runtime objects and
  applies a case's `compiler.definitions` only to that case's own compilation,
  so a conformance case structurally cannot exercise a differently configured
  `plat`. Do not manufacture one.
- **Specification trace:** the rule the gates rely on is
  `docs/language/annotations-and-sizeof.md`, "`@cond(condition)`" — a false
  condition removes the declaration before anything else looks at it, so it
  reaches no emitted artifact — together with "Absence, and where it means
  false", which is what makes the negative polarity default-enabled.
- **Negative/diagnostic cases:** none. There is no new diagnostic; a link that
  disables startup and supplies no entry point fails as an ordinary undefined
  symbol, which is the same designed outcome as any unfilled capability.
- **Regression coverage:** `tests/t22_program_entry_point` and
  `tests/t33_panic_handler` are the cases most sensitive to the entry symbol
  and the Windows/libc adapter respectively; the whole suite links through
  `_start` and so covers the default path broadly.
- **Commands:** `just build-runtime-all` then `just test-all` for the default
  path, and `just check-platform` for the new disabled-build checks. The
  Windows and AArch64 targets are compile-and-inspect only, as they already
  are in `bin/check-platform`; no cross link or execution is added.
