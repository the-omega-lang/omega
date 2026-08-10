# Console I/O for `core`/`std`, and the cross-file macro visibility it needs

## Task Description

- **What is being asked:** a real console-I/O subsystem for Omega's standard
  library — console out (stdout/stderr) and console in (stdin) — designed to
  Omega's own commitments rather than bolted onto libc's `printf`. It ships in
  four layers:

  1. **Three `@gap` specs** in `core::glue` (`StandardOutput`,
     `StandardError`, `StandardInput`) — the only platform-specific part.
  2. **`core::io`** — `Writer`, `Reader`, and the four print macros. No
     allocation, no globals, caller-supplied buffers, freestanding-viable.
  3. **`core::fmt`** — the `Display` spec plus the integer/float digit engine;
     `Display` is added to the existing `for`-blocks in `core::numerics` and
     `core::strings`, and to two new ones for `bool` and `char`.
  4. **`std::io`** — the allocation-dependent conveniences: a `String`-backed
     `Writer`, `read_line` into a `String`, and `to_string`.

  Prerequisite, in the same plan: **macro definitions must become visible
  across files**, following the same visibility and import rules every other
  item already follows. Without it `println$` cannot leave the file that
  defines it.

  A `plat/libc` glue and a golden-output test harness (the repo has neither an
  `.omg` test mechanism nor any way to observe program output today) complete
  the deliverable.

- **Purpose:** Omega currently has no I/O abstraction whatsoever. Every byte
  the toolchain has ever printed comes from two raw C externs at
  `examples/dev/main.omg:1-2` (`puts`, `printf`), used ~190 times with
  hand-written `<*u8>b"...\0"` literals. That violates three of Omega's stated
  commitments at once: it **requires libc**, it is **not embedded-viable**
  (no `printf` on a freestanding target), and it is **not sound** — a float
  argument to a variadic call reads garbage because Cranelift never sets `%al`
  per the x86-64 SysV convention (`docs/14-known-issues.md:11-27`). It also
  blocks the ecosystem: with no way to print, there is no `.omg` test harness,
  no assertion output, and no diagnostics from a running Omega program.

  This serves *no libc requirement*, *embedded systems as a first-class
  target*, *no hidden behavior*, and *a large ecosystem* — in that order.

- **Reasoning:**

  - **Why a gap, not an extern.** `write(2)`/`read(2)` are POSIX, not
    universal. The gap/glue mechanism is exactly the "one project-wide answer,
    supplied downstream" shape (`docs/21-gaps-and-glue.md`), and
    `runtime/core/core/glue.omg:3-5` already declares that file's charter to be
    new `@gap` specs. `docs/21-gaps-and-glue.md:194-199` names "a logger's
    sink" as a legitimate gap; a console is the same category.

  - **Why three gaps, not one `Console`.** A gap function cannot have a
    default body (`GapFunctionBodyNotYetSupported`), so every function in a gap
    must be implemented by every platform. A UART-only embedded target with no
    stdin would be forced to write a lying stub. Three single-purpose gaps let a
    platform glue exactly the capabilities it has, and a single `@glue` marker
    may implement several gaps at once, so the libc platform still writes one
    marker. Splitting also removes the "which stream is `1`?" magic number: the
    stream is the gap's identity, not a parameter, so an invalid stream is
    unrepresentable.

  - **Why one concrete `Writer` struct, not a `Write` spec.** A spec would mean
    either dynamic dispatch (a vtable, and `docs/14-known-issues.md:60-62`
    records that coercion into `spec *T` is not wired into struct-literal
    fields or array elements — precisely where a writer gets stored) or a
    generic `fmt<W: Write>` on every `Display` implementor (a spec method with
    its own generic parameter, unsupported). A concrete struct carrying a
    function-pointer sink gives full static dispatch at every call, one
    indirect call *per buffer flush* rather than per byte, and stays open to
    user sinks (a UART, a ring buffer, a file) with no compiler feature at all.
    Function-pointer struct fields are proven working:
    `examples/dev/main.omg:78,80,570-572`.

  - **Why buffering is opt-in.** There is no pre-`main` hook and no atexit hook
    (`compiler/omega-codegen/src/cranelift/item.rs:80` simply emits the entry
    function as `main`), so any cross-statement buffer can silently lose a
    trailing partial line with nothing able to catch it. `Writer::stdout()` is
    therefore unbuffered: bytes reach the sink when the call returns, always.
    A caller who wants throughput supplies a buffer explicitly and owns the
    `flush`. This is Zig's answer to the same problem and it is the only one
    consistent with *no hidden behavior*.

  - **Why the print macros buffer anyway — visibly.** An unbuffered
    `println$("x = ", x)` would be three syscalls. The macro's expansion
    declares its own 256-byte stack buffer inside a block, writes into it, and
    flushes before the statement ends: one syscall per print statement, zero
    global state, nothing to forget, and the buffering is *written out in the
    macro body* where anyone can read it. The block also solves hygiene — Omega
    has no gensym, but a block-scoped binding cannot collide with or leak into
    the surrounding code. Bare `{ ... }` is already a legal statement
    (`compiler/omega-parser/src/parser/expression.rs:510-526`,
    `parse_block_shaped_primary`).

  - **Why macro visibility must be fixed first.** `omega_parser::macros::expand`
    takes one `SourceModule` and collects definitions from that file's own item
    list only (`macros.rs:203-235`); the driver calls it per file at
    `omega-driver/src/modules.rs:203`. `core::numerics` only gets away with
    macros because it defines and invokes them in one file. A `println$` in
    `core::io` would be unusable by every consumer. Macros are Omega's only
    abstraction-over-syntax mechanism; one that cannot cross a file boundary is
    not a language feature, it is a local code-generation trick.

  - **Why macro visibility reuses the existing rules rather than inventing
    any.** Macros get `exposed`/`internal`/hidden exactly like every other
    item, resolve through ordinary `import`, and fall back to the `core`
    ambient prelude last — the same three-step every ordinary name already
    follows. No new syntax: the invocation grammar stays `Ident Dollar LParen`,
    because an `import` already binds a bare name. Today's behaviour is the
    new default (`hidden`), so the change is strictly additive and
    `core::numerics`' three macros stay module-private with no edit.

  - **Rejected: keeping `printf`.** Requires libc, is unsound for floats, has
    no length-aware string primitive (`docs/11-strings-casting-and-slices.md:84-87`
    documents the `%.*s` dance a non-NUL-terminated `*str` forces), and gives
    zero compile-time type checking.

  - **Rejected: `{}` format strings.** Macro arguments are raw token runs and a
    string literal is a single token — the expander never inspects its text, so
    `println$("x = {}", x)` cannot be split at compile time. The reachable
    ceiling is concatenative, `println$("x = ", x)`. A real `{}` syntax would
    have to be a compiler intrinsic splitting the literal during analysis; that
    is a separate, later decision and this design does not block it (such an
    intrinsic would lower to exactly the same `Display::fmt` calls).

  - **Rejected: a runtime format-string interpreter** (`format(fmt, &[Arg::…])`).
    It needs a tagged `Arg` union, defeats static dispatch, moves arity errors
    from compile time to runtime, and puts a parser in the output path. Wrong
    on cost, wrong on safety, wrong for embedded.

- **Resolved concerns** (raised in review, settled with the user before
  planning):

  - **Macros are file-local.** Decision: **fix it in this plan**, as Part A,
    before any I/O code depends on it.
  - **`{}` format strings are unreachable via macros.** Accepted; the API is
    concatenative.
  - **Buffering policy.** Decision: **unbuffered by default**, explicit
    buffered writer, macros buffer visibly in their own expansion.
  - **Float formatting.** Decision: **fixed precision now** — six fractional
    digits, `nan`/`inf`/`-inf` handled, scientific fallback outside the
    fixed-notation range, documented as deliberately not round-trip.
  - **Chaining vs. plain methods.** Decision: **no chaining** (write methods
    return `void`, matching `List::push` and the rest of the house style); the
    variadic macro is what supplies ergonomics.

## Technical Details

### What changes

**Part A — cross-file macro visibility (compiler)**

| Area | File | Change |
| --- | --- | --- |
| Macro AST | `compiler/omega-parser/src/ast/statement/macro_definition.rs` | `MacroDefinitionStmt` gains `pub visibility: Visibility` |
| Macro parsing | `compiler/omega-parser/src/parser/macro_syntax.rs` | `parse_macro_definition` takes and stores `visibility: Visibility` |
| Item parsing | `compiler/omega-parser/src/parser/item.rs` | delete the `reject_visibility` call in the `TokenKind::Macro` arm (~line 156); pass `visibility` through. The macro-*invocation* arm keeps its `reject_visibility` |
| Diagnostics | `compiler/omega-parser/src/diagnostics.rs` | extend `VisibilityNotAllowedHere`'s help text (~line 99) to include macros in the allowed list |
| Expansion | `compiler/omega-parser/src/macros.rs` | `expand` gains an `imported` parameter; `collect_definitions` merges it; `expand_item_list`'s `Item::MacroDefinition` arm returns a real error instead of `unreachable!()` |
| Driver — AST cache | `compiler/omega-driver/src/modules.rs` | `ModuleStore` gains `asts`; new `Driver::ensure_ast`; `parse_module` restructured to `ensure_ast` → `macro_env` → `expand` → `lower` |
| Driver — macro env | `compiler/omega-driver/src/modules.rs` | new `Driver::macro_env`, `Driver::module_macros`, `Driver::prelude_macros`; `Driver` gains a memo field for the prelude |
| Driver — path arith | `compiler/omega-driver/src/modules.rs` | `import_absolute_path` takes a precomputed `base: &[Ident]`; `relative_base` split so the base can be computed from a `ModuleLocation` before the module is in the store |
| Driver — errors | `compiler/omega-driver/src/error.rs` | new `CompileError` variant for two `core` modules exposing the same macro name |
| Sources | `runtime/core/core/numerics.omg` | none — its three macros stay hidden, which is the new default |

**Part B — the I/O library**

| Area | File | Change |
| --- | --- | --- |
| Gaps | `runtime/core/core/glue.omg` | add `@gap StandardOutput`, `@gap StandardError`, `@gap StandardInput` |
| Writers/readers | `runtime/core/core/io.omg` (new) | `Writer`, `Reader`, console constructors, `print$`/`println$`/`eprint$`/`eprintln$` |
| Formatting | `runtime/core/core/fmt.omg` (new) | `spec Display`, `write_uint`, `write_int`, `write_float`, `write_bool`, `write_char` |
| Primitive `Display` | `runtime/core/core/numerics.omg` | add `Display` to all three macro-generated `for`-blocks (`SignedIntegerOps`, `UnsignedIntegerOps`, `FloatOps`) plus a `fmt` body each |
| Primitive `Display` | `runtime/core/core/strings.omg` | add `Display` to `StrOps` plus a `fmt` body |
| Primitive `Display` | `runtime/core/core/chars.omg` (new) | `CharOps : Display for char` |
| Primitive `Display` | `runtime/core/core/bools.omg` (new) | `BoolOps : Display for bool` |
| Glue | `runtime/plat/libc/libc.omg` | `internal extern write`/`read`; `@glue marker LibcConsole` implementing all three gaps |
| Std conveniences | `runtime/std/std/io.omg` (new) | `string_writer`, `read_line`, `to_string<T: Display>` |
| Build | `justfile` | new `build-std`; register `--extern=std:` on the example; link `target/std.o`; new `build-io-demo`/`test-io` |
| Test target | `examples/io_demo/` (new) | the golden-output integration program |
| Golden files | `tests/io_demo.stdin`, `tests/io_demo.expected` (new) | fixed input and expected stdout |
| Tests | `compiler/omega-parser/tests/macros.rs` | new cases for imported-macro merging, shadowing, visibility parsing |
| Docs | `docs/24-console-io.md` (new); updates to `07`, `10`, `12`, `13`, `14`, `21`, `22`, `23`, `README.md` | see the docs step |

### What must not change

- **`examples/dev/main.omg`.** It is the de facto integration test for
  everything else in the language (~1500 lines, ~190 `printf` call sites). Do
  **not** migrate it to the new I/O. The only edit it may receive is in Part A's
  verification step, and only if a cross-file macro demo is added there
  deliberately. If an unrelated failure appears there, report it — do not work
  around it.
- **`runtime/shims/x86_64-unknown-linux.S`.** Not linked by any real recipe
  today; leave it exactly as is. A freestanding console glue is a separate,
  later package under `runtime/plat/`.
- **The macro expansion model.** Still a pure `SourceModule -> SourceModule`
  transform run before HIR lowering, still no hygiene, no gensym, no
  macro-specific type checking, still duck-typed at the call site. Part A adds
  *name resolution*, nothing else. `docs/12-macros.md`'s "Duck-typed expansion"
  and "Why no gensym/hygiene machinery exists" sections stay true verbatim.
- **The invocation grammar.** `name$(args)` only. No qualified
  `core::io::println$(...)` form — an `import` already binds a bare name, and
  `core` is ambient.
- **`MacroError`'s span-less shape.** It carries names, not spans
  (`docs/plan/0001-impl-macro-varargs.md:109-114` deliberately left this). Do
  not start threading spans through it here.
- **The out-pointer/`bool` vs `Option<T>` split.** `core::io`'s fallible reads
  use out-pointer + `bool` (`core::slices`' documented doctrine,
  `runtime/core/core/slices.omg:9-14`). Do not introduce `Result<T, E>`; nothing
  here needs a payload-bearing error.
- **`std`'s existing five collections.** Untouched except that `std::io` is a
  new sibling module.
- **`core::numerics`' three existing macros.** They stay hidden (no modifier),
  which is exactly their current behaviour under Part A's default.

### Chosen approach

#### A1. Macro visibility: the rules

A macro definition takes an ordinary visibility modifier and means the ordinary
thing (`docs/07-visibility.md`):

- **hidden** (no modifier, the default) — visible only inside its own file.
  Identical to today's behaviour for every existing macro.
- **`internal`** — visible anywhere in the same top-level package (same first
  path segment).
- **`exposed`** — visible anywhere.

An invocation `name$(...)` resolves `name` in this order, first match wins:

1. a macro defined in the same file (any visibility);
2. a macro bound by an `import` in this file;
3. an `exposed` macro in any `core` module (the ambient prelude).

Rule 1 shadowing rule 2/3 mirrors how ordinary names already work — the ambient
`core` prelude is the last-resort fallback, never an override.

Two constraints fall out and must be documented, not worked around:

- **Macro visibility is not transitive.** Building module `M`'s macro
  environment reads only `M`'s *own* import statements and each target's *own*
  definitions — never the target's imports. This matches the language having no
  re-export concept at all (`docs/07-visibility.md:143-150`), and it is what
  makes the whole pre-pass acyclic and recursion-free.
- **A macro body's nested invocations resolve at the call site**, not at the
  definition site, because expansion is textual splicing. A macro in `core::io`
  therefore may not invoke a helper macro that is not itself visible to the
  caller. This is the existing duck-typed-expansion model applied unchanged, and
  the print macros are written flat so they never need one. Record it in
  `docs/12-macros.md` and `docs/14-known-issues.md` as a known limitation with
  its fix shape (a per-definition home environment).

#### A2. Macro visibility: the split of labour

The parser stays dumb; the driver owns resolution. `expand`'s new signature:

```rust
pub fn expand(
    module: SourceModule,
    imported: &HashMap<Ident, MacroDefinitionStmt>,
) -> Result<SourceModule, MacroError>
```

`collect_definitions` builds the file's own map exactly as today (so
`DuplicateMacroDefinition` still fires only for two definitions in one file),
then produces the merged map as `imported.clone()` extended with the local one —
local wins. Everything downstream is untouched: all 17 `expand_*` functions keep
`defs: &HashMap<Ident, MacroDefinitionStmt>` verbatim, and the three
`defs.get(&inv.name)` lookup sites (`macros.rs:375, 410, 447`) need no edit.
Blast radius inside `macros.rs` is `expand` plus `collect_definitions`.

`validate_definition` continues to run over the *merged* map. Validating an
imported definition again in each importing module is cheap and idempotent, and
it closes the hole where a module is scanned for macros but never itself
expanded.

#### A3. Macro visibility: the driver pre-pass

Three new memoized `Driver` methods in `modules.rs`:

- `module_macros(&mut self, path: &[Ident]) -> Result<Rc<HashMap<Ident, MacroDefinitionStmt>>, ResolveError>`
  — `ensure_ast(path)`, then scan `nodes` for `Item::MacroDefinition`, cloning
  each into a map. Never expands, never recurses.
- `prelude_macros(&mut self) -> Result<Rc<HashMap<Ident, MacroDefinitionStmt>>, ResolveError>`
  — computed once per compilation, memoized on a new `Option<Rc<…>>` field on
  `Driver`. Walks `self.roots.core_modules()` (`roots.rs:238-246`) and collects
  every `exposed` macro. Two `core` modules exposing the same macro name is a
  hard `CompileError` naming both modules.
- `macro_env(&mut self, path: &[Ident], directory_shaped: bool) -> Result<HashMap<Ident, MacroDefinitionStmt>, ResolveError>`
  — starts from `prelude_macros()` (minus `path`'s own entry if `path` is itself
  a `core` module), then for each `Item::Import` in `path`'s raw AST:
  1. `base = relative_base_for(path, directory_shaped)`;
  2. `abs = import_absolute_path(path, &base, import.root, &import.path)`;
  3. if `self.roots.module_exists(&abs)` this is a whole-module import and binds
     no macro (there is no qualified invocation syntax) — skip;
  4. otherwise split `abs` into `(module, item)`; if `module_exists(module)`,
     look `item` up in `module_macros(module)` and bind it if its visibility
     permits (`exposed` always; `internal` when `path[0] == module[0]`; hidden
     never).

  A failure to locate an imported module is *not* reported here — the ordinary
  import-resolution path already reports it with a proper span
  (`index_imports`, `modules.rs:314-323`). Swallow the `ResolveError` and move
  on, so no diagnostic is ever emitted twice.

  Two imports binding the same macro name likewise needs no new error: two
  imports with the same alias already produce `Redeclaration` in
  `index_imports` (`modules.rs:333-345`). Bind the first and let that fire.

`import_absolute_path` (`modules.rs:420-457`) currently calls
`self.relative_base(importer)`, which reads `self.modules.parsed(module_path)` —
unavailable during the pre-pass, because the module is not in the store yet.
Split it: keep `relative_base(&self, module_path)` as a thin wrapper that reads
`directory_shaped` from the store and delegates to a new
`relative_base_for(&self, module_path: &[Ident], directory_shaped: bool)`, and
change `import_absolute_path` to take `base: &[Ident]` as a parameter.
`index_imports` computes it once via `relative_base` and passes it in; the
pre-pass computes it from `location.children_dir.is_some()`, which
`parse_module` already has in hand at `modules.rs:185`.

#### A4. Macro visibility: the AST cache

`parse_module` (`modules.rs:180-222`) becomes:

```
locate → ensure_ast (read + parse + cache raw SourceModule)
       → macro_env  (needs the raw AST's imports)
       → expand(ast, &env)
       → lower_module
```

`ModuleStore` gains `asts: HashMap<ModulePath, Rc<SourceModule>>`, holding the
**unexpanded** AST. `ensure_ast` owns the `std::fs::read_to_string`, the
`sources` insert, and the `LoadFailure::Parse` stash currently inline at
`modules.rs:190-202`. A module scanned for macros and later compiled is parsed
exactly once. Raw ASTs stay alive for the compilation; that is a few hundred
kilobytes for `core` + `std` + an application and is the price of not parsing
twice.

Two ordering facts to document rather than engineer around:

- A macro that **expands to an `import`** produces a real import node
  (`expand_items_invocation` re-parses through the full `parse_source_module`),
  but macro resolution ran before expansion, so that import cannot contribute
  macros. State it in `docs/12-macros.md`.
- A macro that **expands to a macro definition** currently panics the compiler
  (`macros.rs:357-359`, `unreachable!()` — reachable, because the item-position
  re-parse admits `TokenKind::Macro`). Replace it with a proper
  `MacroError::MacroDefinitionInExpansion { macro_name }`. This is a live panic
  in code this plan already edits; fixing it here is cheaper than leaving it.

#### B1. The gaps

In `runtime/core/core/glue.omg`, following the file's existing shape (`@gap` on
its own line, self-less functions, `*mut` only where the callee writes):

```
@gap
exposed spec StandardOutput {
    write(bytes: *[?]u8, written: *mut usize) => bool;
}

@gap
exposed spec StandardError {
    write(bytes: *[?]u8, written: *mut usize) => bool;
}

@gap
exposed spec StandardInput {
    read(into: *mut [?]u8, count: *mut usize) => bool;
}
```

Out-pointer + `bool` is `core`'s documented convention for a fallible,
no-allocation operation (`slices.omg:9-14`). `false` means the platform reported
an error and leaves `*written`/`*count` untouched. `true` with `*count == 0`
from `StandardInput::read` means end of input. A short write is normal and
reported through `*written`; the `Writer` loops.

#### B2. `core::io::Writer`

```
exposed struct Writer {
    sink: (ctx: usize, data: *[?]u8) => usize;
    ctx: usize;
    buf: *mut u8;
    cap: usize;
    len: usize;
    failed: bool;
}
```

One type, three roles, distinguished by data rather than by a tag:

- `cap == 0` — unbuffered: every write goes straight to `sink`.
- `cap > 0` — buffered: writes accumulate; a full buffer flushes; `flush` is the
  caller's job.
- `sink` null (`<(ctx: usize, data: *[?]u8) => usize>0`) — memory only:
  overflow sets `failed` instead of flushing. This is what `std::io`'s
  `String` writer and any in-memory formatting use.

The sink returns the number of bytes actually consumed. `flush` loops while
progress is made; a sink returning `0` with bytes still pending sets `failed`
and discards the remainder — the only honest option in a language with no
panic and no `Result`. `failed` is sticky and readable via `had_error()`.

Constructors (all thin wrappers over one private `Self::new`):

```
exposed to_sink(sink: (ctx: usize, data: *[?]u8) => usize, ctx: usize, buffer: *mut [?]u8) => Self
exposed to_buffer(buffer: *mut [?]u8) => Self          # null sink, memory only
exposed stdout() => Self                                # unbuffered
exposed stderr() => Self                                # unbuffered
exposed stdout_buffered(buffer: *mut [?]u8) => Self
exposed stderr_buffered(buffer: *mut [?]u8) => Self
```

Methods: `write_bytes(*mut self, data: *[?]u8) => void`,
`write_byte(*mut self, b: u8) => void`,
`write_str(*mut self, s: *str) => void` (a free `<*[?]u8>s` reinterpret),
`newline(*mut self) => void`, `flush(*mut self) => void`,
`had_error(*self) => bool`. All return `void` — the house style
(`List::push`), and the macros are what make that ergonomic.

The two console sinks are module-private free functions in `core::io`:

```
stdout_sink(ctx: usize, data: *[?]u8) => usize {
    mut written : usize = 0;
    if StandardOutput::write(data, &mut written) { written } else { 0 }
}
```

`ctx` is unused for the console sinks but is what keeps `Writer` open to user
sinks (an fd, a UART base address, a ring-buffer pointer) with no compiler
feature at all.

#### B3. `core::io::Reader`

Mirror-image, plus a read cursor:

```
exposed struct Reader {
    source: (ctx: usize, into: *mut [?]u8) => usize;
    ctx: usize;
    buf: *mut u8;
    cap: usize;
    len: usize;
    pos: usize;
    eof: bool;
    failed: bool;
}
```

`Reader::stdin()`, `Reader::stdin_buffered(buffer)`,
`Reader::from_source(source, ctx, buffer)`.

- `read(*mut self, into: *mut [?]u8, out: *mut usize) => bool` — bulk read;
  `true` with `*out == 0` is end of input.
- `read_byte(*mut self, out: *mut u8) => bool`.
- `read_line(*mut self, into: *mut [?]u8, out: *mut usize) => bool` — fills
  `into` up to but not including `\n`, consuming the `\n`; `*out` is the byte
  count. Returns `false` on error **or** when the line does not fit, so a
  truncating success is never silently reported as success.

Buffering matters more here than for output: a console reader cannot un-read,
so an unbuffered `read_line` must issue one syscall per byte. That is correct
and it is what an unbuffered reader means — document it plainly, and point at
`Reader::stdin_buffered` for real work. Predictable cost the programmer can see
is the commitment; hiding it is not.

#### B4. `core::fmt`

```
exposed spec Display {
    fmt(*self, out: *mut Writer) => void;
}
```

plus the engine, all `exposed` free functions so callers can reach a base
directly:

- `write_uint(out: *mut Writer, value: u64, base: u32) => void` — digits built
  backwards into a `[64]u8` local (worst case: base 2), then written as one
  slice `&digits[n..]`. Lowercase for bases above 10.
- `write_int(out: *mut Writer, value: i64, base: u32) => void` — sign, then the
  magnitude through `write_uint`. Compute the magnitude in `u64` so `i64`'s most
  negative value does not overflow on negation.
- `write_float(out: *mut Writer, value: f64) => void` — see below.
- `write_bool`, `write_char` (`char` encoded as 1–4 UTF-8 bytes; reuse the exact
  shift/mask encoding already written in `runtime/std/std/string.omg:56-74`).

Float formatting, fixed precision, in this order:

1. Reinterpret to `u64` (`*<*u64>&value`, the same bit-reinterpret idiom
   `numerics.omg` already uses for hashing) and test the exponent field: all
   ones with a zero mantissa is `inf`/`-inf`, all ones with a non-zero mantissa
   is `nan`. Exact, and it needs no unrepresentable float constant.
2. Emit the sign; work with the magnitude.
3. If the magnitude is `>= 1.0e19` (past `u64`'s range) or non-zero and
   `< 1.0e-6` (where six fixed digits would print `0.000000` and destroy the
   value), use scientific notation: normalize by repeated multiply/divide by
   ten while counting a decimal exponent, then print `d.dddddde±NN`.
4. Otherwise: add `0.5e-6` for round-to-nearest, split into integer and
   fractional parts, print the integer part via `write_uint`, a `.`, then six
   zero-padded fractional digits.

Document it as deliberately fixed-precision, not shortest-round-trip; a Ryu- or
Grisu-class algorithm is a named follow-up.

#### B5. Attaching `Display` to primitives

A target type gets exactly one `for`-block, globally
(`docs/13-core-library.md:79-83`), so `Display` is added to the *existing*
blocks, never a competing one:

- `runtime/core/core/numerics.omg` — each of the three macros gains `Display` in
  its spec list and one `fmt` body:
  `signed_integer` → `write_int(out, <i64>*self, 10u32);`,
  `unsigned_integer` → `write_uint(out, <u64>*self, 10u32);`,
  `float_ops` → `write_float(out, <f64>*self);`.
- `runtime/core/core/strings.omg` — `StrOps` gains `Display`; body is
  `out.write_bytes(<*[?]u8>self);`. **Write the cast inline; do not call
  `self.as_bytes()`.** A `for`-attached method calling a sibling extension
  method on the same type loses visibility once the type is used from a
  consuming `--extern` package (`docs/14-known-issues.md:96-107`) — exactly what
  would happen to every consumer of `"...".fmt(...)`. Every primitive `fmt` body
  in this design calls only free functions and `Writer` methods, never a sibling
  extension method, and that is deliberate.
- `runtime/core/core/chars.omg` (new) — `CharOps : Display for char`. Keep it
  to `Display` only; the ASCII classification/case module
  `docs/13-core-library.md:171-186` anticipates is separate work.
- `runtime/core/core/bools.omg` (new) — `BoolOps : Display for bool`.

#### B6. The print macros

In `runtime/core/core/io.omg`, `exposed`, so the ambient `core` prelude carries
them everywhere with no import:

```
exposed macro println($args: expr...) => {
    {
        mut buf : [256]u8;
        mut out := Writer::stdout_buffered(&mut buf[0..]);
        $...(){ $args.fmt(&mut out); }
        out.newline();
        out.flush();
    }
}
```

and `print$` (no `newline`), `eprint$`/`eprintln$` (`stderr_buffered`). Four
macros, each five lines, no nested macro invocation.

Every name a macro body uses must resolve **at the call site**. `Writer` and
`Display` live in `core`, which is ambient everywhere, so that holds with no
import anywhere in user code. `&mut buf[0..]` yields `*mut [?]u8` directly from
an array local (`examples/dev/main.omg:1030`), so no cast is needed. `defer` is
deliberately not used: it fires at *function* exit, not block exit
(`docs/00-functions.md:106-110`), so the explicit `flush` is both correct and
necessary.

#### B7. The libc glue

In `runtime/plat/libc/libc.omg`, beside the existing `LibcAllocator`:

```
internal extern write : (fd: i32, buf: *u8, count: usize) => isize;
internal extern read : (fd: i32, buf: *mut u8, count: usize) => isize;

@glue
exposed marker LibcConsole
    : core::glue::StandardOutput, core::glue::StandardError, core::glue::StandardInput { ... }
```

Bodies drop the fat pointer to a thin one plus a count — `<*u8>bytes` is the
existing `DropLength` cast and `bytes.length` is the slice's own `i32` leaf —
then call `write(1, …)` / `write(2, …)` / `read(0, …)`. A negative return is
`false`; otherwise `*written = <usize>n; true`. The raw syscall wrappers are
used, never `printf`/`fwrite`: no format-string parsing, no `FILE*` (an
`extern` *data* declaration would hit
`compiler/omega-codegen/src/cranelift/function.rs:105`'s `todo!()`), no float
ABI hazard, and a length-taking primitive that suits a non-NUL-terminated
`*str` exactly.

A second marker rather than extending `LibcAllocator`: a glue may implement
several gaps, but heap and console are unrelated capabilities and a platform
may well have one without the other.

#### B8. `std::io`

Three items, following `std::alloc`'s precedent of a thin non-generic wrapper:

- `string_writer(s: *mut String) => Writer` — a `Writer` whose sink appends to
  a `String`, with the `String` pointer carried in `ctx` (pointer↔`usize` casts
  are free). This is how any value gets formatted into owned memory, with no
  new machinery at all.
- `read_line(reader: *mut Reader, into: *mut String) => bool` — the growable
  counterpart to `core`'s fixed-buffer version; no length limit.
- `to_string<T: Display>(value: T) => String` — construct, `string_writer`,
  `value.fmt(...)`, return. Caller owns the `String` and `defer`s its `free`,
  per `std`'s universal ownership idiom.

### Risks and open questions

- **`&mut buf[0..]` inside a macro expansion.** Verified at
  `examples/dev/main.omg:1030` in hand-written code. If it misbehaves
  specifically under statement-position splicing, report it rather than
  switching the macro to a named buffer — a hygiene-free named local is the
  thing the block exists to avoid.
- **`[N]T` sizes must be bare decimal literals**
  (`compiler/omega-parser/src/parser/type.rs:105-121`) — no `comp` constant.
  Every buffer size in this design is written as digits at its use site. Do not
  try to introduce a shared `BUF_SIZE`.
- **Statement-position macro spans are composite**, stretching from call site to
  definition site (`docs/14-known-issues.md:110-131`). `println$` will be the
  most-used macro in the language, so this pre-existing diagnostic wart is about
  to become far more visible. Do not attempt the fix here — it needs a single
  span policy shared by all three positions. Re-flag it in the known-issues
  entry as now-higher-priority.
- **Building `core` standalone will warn about three more unfilled gaps.** True
  and expected, exactly as `GlobalAllocator` already does. Do not silence it.
- **Float scientific-notation fallback loses precision** by construction
  (repeated multiply/divide by ten). Acceptable under the fixed-precision
  decision; state it in the docs rather than hiding it.
- **`std` is currently built by no recipe at all** and `--extern=std:` appears
  nowhere. Part B wires it up for the first time; expect first-time breakage in
  `std` itself and report it separately rather than folding unrelated fixes into
  this work.
- **Generic method with a spec bound calling a bound method** (`to_string<T:
  Display>` calling `value.fmt(...)`) mirrors `HashMap<K: Hash>` calling
  `key.hash()`, which works. If it does not, drop `to_string` from this pass and
  report it — do not redesign `Display` around it.

## Implementation Plan

Each step leaves `cargo build` green and `just build-exe` working.

### Part A — cross-file macro visibility

**Step 1 — visibility on a macro definition.**
1. `compiler/omega-parser/src/ast/statement/macro_definition.rs`: add
   `pub visibility: Visibility` to `MacroDefinitionStmt`, documented as
   following the ordinary three-level rule.
2. `compiler/omega-parser/src/parser/macro_syntax.rs`: change
   `parse_macro_definition(p: &mut Parser)` to
   `parse_macro_definition(p: &mut Parser, visibility: Visibility)` and store it.
3. `compiler/omega-parser/src/parser/item.rs`: in the `TokenKind::Macro` arm
   (~line 154-158) delete the `reject_visibility` call and pass `visibility`
   through. Leave the macro-*invocation* arm's `reject_visibility` alone.
4. `compiler/omega-parser/src/diagnostics.rs`: add macros to
   `VisibilityNotAllowedHere`'s help text (~line 99), which currently enumerates
   the allowed item kinds and would otherwise lie.
5. `cargo build`; `just build-exe` (nothing in the runtime uses a modifier yet,
   so behaviour is unchanged).

**Step 2 — `expand` accepts imported definitions.**
1. `compiler/omega-parser/src/macros.rs`: change `expand` to
   `pub fn expand(module: SourceModule, imported: &HashMap<Ident, MacroDefinitionStmt>) -> Result<SourceModule, MacroError>`.
   Build the file's own map with the existing `collect_definitions` (unchanged,
   so `DuplicateMacroDefinition` stays file-local), then merge:
   `let mut defs = imported.clone(); defs.extend(own);`. Keep the
   `for def in defs.values() { validate_definition(def)?; }` loop over the
   merged map.
2. Same file: replace the `Item::MacroDefinition(_) => unreachable!(...)` arm in
   `expand_item_list` (~line 357) with a new
   `MacroError::MacroDefinitionInExpansion { macro_name: Ident }`, plus its
   `Display` arm. Note in its doc comment that this is reachable because the
   item-position re-parse admits `TokenKind::Macro`.
3. `compiler/omega-driver/src/modules.rs:203`: pass `&HashMap::new()` for now.
4. `compiler/omega-parser/tests/macros.rs`: update the local `expand` helper
   (lines 8-10) to pass an empty map. All six existing tests must pass
   unchanged.

**Step 3 — the driver's raw-AST cache.**
1. `compiler/omega-driver/src/modules.rs`: add
   `asts: HashMap<ModulePath, Rc<SourceModule>>` to `ModuleStore`, documented as
   holding the **unexpanded** AST.
2. Extract `Driver::ensure_ast(&mut self, path: &[Ident], file: &std::path::Path) -> Result<Rc<SourceModule>, ResolveError>`
   from the body currently inline at `modules.rs:190-202`: read the file, insert
   into `sources`, parse, stash `LoadFailure::Parse` on failure, cache and
   return. Memoize on `asts`.
3. Rewrite `parse_module` (`modules.rs:180-222`) to call `ensure_ast` and then
   `expand(ast, &HashMap::new())`, keeping every other behaviour identical.
4. `cargo build`; `just build-exe`.

**Step 4 — import path arithmetic usable before a module is in the store.**
1. `compiler/omega-driver/src/modules.rs`: add
   `fn relative_base_for(&self, module_path: &[Ident], directory_shaped: bool) -> ModulePath`
   holding the current body of `relative_base` (lines 408-414); make
   `relative_base` a wrapper that reads `directory_shaped` from the store.
2. Change `import_absolute_path` (line 420) to take `base: &[Ident]` and use it
   in the `ImportRoot::Local` arm instead of calling `relative_base` itself.
3. Update its one caller, `index_imports` (line 314), to compute
   `let base = self.relative_base(path);` once before the loop.
4. `cargo build`; `just build-exe`.

**Step 5 — the macro environment.**
1. `compiler/omega-driver/src/modules.rs`: add
   `module_macros(&mut self, path: &[Ident]) -> Result<Rc<HashMap<Ident, MacroDefinitionStmt>>, ResolveError>`
   — `roots.locate(path)`, `ensure_ast`, scan `nodes` for
   `Item::MacroDefinition`, memoized in a new `ModuleStore` field.
2. Add `prelude_macros(&mut self) -> Result<Rc<HashMap<Ident, MacroDefinitionStmt>>, ResolveError>`
   — memoized once on a new `Driver` field; walks `self.roots.core_modules()`
   and collects every `Visibility::Exposed` macro.
3. `compiler/omega-driver/src/error.rs`: add
   `CompileError::AmbiguousPreludeMacro { name: Ident, first: ModulePath, second: ModulePath }`
   with a message naming both `core` modules, plus its rendering arm.
4. Add `macro_env(&mut self, path: &[Ident], directory_shaped: bool) -> Result<HashMap<Ident, MacroDefinitionStmt>, ResolveError>`
   implementing A3's four-step import walk. Swallow a per-import
   `ResolveError` (the ordinary import path reports it with a span); bind the
   first of two same-named imports (the ordinary path reports `Redeclaration`).
5. Wire it: in `parse_module`, between `ensure_ast` and `expand`, call
   `macro_env(path, location.children_dir.is_some())` and pass the result to
   `expand`.
6. `cargo build`; `just build-exe`.

**Step 6 — prove Part A end to end.**
1. `compiler/omega-parser/tests/macros.rs`: add tests that pass a non-empty
   `imported` map — an imported macro invoked with no local definition; a local
   definition shadowing an imported one of the same name; two local definitions
   still reporting `DuplicateMacroDefinition`; an unknown macro still reporting
   `UnknownMacro`; `exposed macro m() => { ... }` parsing and recording
   `Visibility::Exposed`; a macro expanding to a macro definition reporting
   `MacroDefinitionInExpansion` instead of panicking.
2. Cross-file behaviour is proven by the runtime build in Part B (an `exposed`
   macro in `core::io` invoked from `examples/io_demo`), which is a stronger
   check than any unit test and needs no new harness.

### Part B — the I/O library

**Step 7 — the gaps.** Add the three `@gap` specs of B1 to
`runtime/core/core/glue.omg`, with a header paragraph in the file's established
style explaining the out-pointer/`bool` channel, why the three are separate, and
what `true` with a zero count means. `just build-core` must succeed and warn
about three newly unfilled gaps.

**Step 8 — `core::io`: `Writer`.** New `runtime/core/core/io.omg` with the
struct, the private `Self::new`, the six constructors, the two console sinks,
and the write/flush methods of B2. Do not add the macros yet. `just build-core`.

**Step 9 — `core::fmt`.** New `runtime/core/core/fmt.omg` with `spec Display`
and the digit engine of B4, floats included. `just build-core`.

**Step 10 — `Display` for primitives.** Extend the three macros in
`runtime/core/core/numerics.omg` and `StrOps` in
`runtime/core/core/strings.omg`; add `runtime/core/core/chars.omg` and
`runtime/core/core/bools.omg`. Every `fmt` body calls only free functions and
`Writer` methods — never a sibling extension method. `just build-core`.

**Step 11 — `core::io`: `Reader`.** Add the struct, constructors and three read
methods of B3 to `runtime/core/core/io.omg`. `just build-core`.

**Step 12 — the print macros.** Add the four `exposed` macros of B6 to
`runtime/core/core/io.omg`. `just build-core`.

**Step 13 — the libc glue.** Add the two `internal extern`s and the
`LibcConsole` marker of B7 to `runtime/plat/libc/libc.omg`, extending that
file's header comment. `just build-plat`.

**Step 14 — `std::io`.** New `runtime/std/std/io.omg` with the three items of
B8. Add a `build-std` recipe to the `justfile` using the invocation already
recorded at `docs/23-standard-library.md:31`:
`./target/debug/omgc runtime/std/ --name=std --extern=core:runtime/core/ -o target/std.o`.
Run it; report any pre-existing `std` breakage separately rather than fixing it
inline.

**Step 15 — the integration program and golden output.**
1. New `examples/io_demo/main.omg`: prints via `println$`/`print$` at least one
   value of every integer width, a float (including `nan`, `inf`, a fractional
   value and a scientific-range value), `bool`, `char`, `*str`, and a
   user-defined struct implementing `Display`; exercises an explicitly buffered
   `Writer` with a `flush`; formats into a `String` through
   `std::io::string_writer` and prints the result; writes one line to stderr via
   `eprintln$`; reads two lines from stdin with `std::io::read_line` and echoes
   them. Returns `0`.
2. New `tests/io_demo.stdin` (the two input lines) and
   `tests/io_demo.expected` (exact expected stdout).
3. `justfile`: a `build-io-demo` recipe compiling `examples/io_demo/` with
   `--extern=core:`, `--extern=std:`, `--extern=plat:runtime/plat/libc/` and
   linking `main.o core.o std.o plat.o`; and a `test-io` recipe running it with
   `tests/io_demo.stdin` on stdin and `diff`ing stdout against
   `tests/io_demo.expected`, exiting non-zero on any difference.
4. Run `just test-io` until it passes. **This is the deliverable's proof** —
   the language has had no way to observe program output until now.

**Step 16 — documentation.**
1. New `docs/24-console-io.md` covering: the three gaps and why three; the one
   concrete `Writer`/`Reader` and why not a spec; unbuffered-by-default and the
   missing-atexit reasoning; `Display` and where each primitive's impl lives;
   fixed-precision floats stated as a deliberate non-round-trip choice; the four
   macros and what they expand to; the `std::io` layer; the build/link lines;
   and a Caveats section.
2. `docs/12-macros.md`: rewrite the scope section for cross-file visibility —
   the three-step resolution order, visibility modifiers on `macro`,
   non-transitivity, nested invocations resolving at the call site, and
   macro-generated imports not contributing macros.
3. `docs/07-visibility.md`: macros now take a modifier.
4. `docs/10-modules-and-linkage.md`: macros participate in the ambient `core`
   prelude and in ordinary imports.
5. `docs/13-core-library.md`: add `core::io`, `core::fmt`, `core::chars`,
   `core::bools` to the layout and API surface; note that `Display` extends the
   existing `for`-blocks rather than adding competing ones, and remove the "No
   `char` module yet" section's now-stale parts without overstating what
   `core::chars` covers (`Display` only).
6. `docs/21-gaps-and-glue.md` and `docs/22-platform-glue.md`: the three new gaps
   and `LibcConsole`.
7. `docs/23-standard-library.md`: `std::io`, and replace the "No `just
   build-std` recipe exists yet" paragraph now that one does.
8. `docs/14-known-issues.md`: add the two new macro limitations
   (non-transitive visibility; nested invocations resolving at the call site),
   note the raised priority of the composite-span issue, and add the
   fixed-precision float limitation. Move nothing that is not actually fixed.
9. `docs/README.md`: add entry 24 to the reading order.

## Testing

### New cases

- **Step 1:** `exposed macro m(...)` and `internal macro m(...)` parse and record
  the right `Visibility`; a macro invocation still rejects a modifier.
- **Step 2/6 (`compiler/omega-parser/tests/macros.rs`):** an imported macro
  invoked with no local definition expands; a local definition of the same name
  shadows the imported one; two local definitions still report
  `DuplicateMacroDefinition`; an unknown name still reports `UnknownMacro`; a
  macro expanding to a macro definition reports `MacroDefinitionInExpansion`.
- **Step 6 (end to end):** `examples/io_demo` invoking `println$` — defined
  `exposed` in `core::io`, never imported anywhere — compiles and runs. This is
  the real proof that Part A works across a package boundary.
- **Step 15 (`just test-io`, golden output):** every integer width at its
  boundary values (including `i64`'s most negative, which must not overflow
  during negation); `write_uint` at bases 2, 10 and 16; `f64` covering `nan`,
  `inf`, `-inf`, `0.0`, `-0.0`, a value needing rounding at the sixth decimal, a
  value below `1.0e-6`, and a value above `1.0e19`; `bool`; a multi-byte `char`;
  a `*str` containing no NUL and no trailing NUL; a user struct implementing
  `Display`; an explicitly buffered writer whose output appears only after
  `flush`; a `String` built through `string_writer`; `read_line` consuming two
  lines including the final one, and its behaviour at end of input.
- **`Reader::read_line` truncation:** a line longer than the supplied buffer
  returns `false` rather than silently truncating.
- **`Writer` error path:** a `to_buffer` writer overflowing its buffer sets
  `had_error()` and does not write out of bounds.

### Negative cases

- `exposed macro m() => { ... }` in a **hidden** position invoked from another
  package must fail with `UnknownMacro`, not silently resolve — i.e. a *hidden*
  macro is genuinely invisible outside its file.
- An `internal` macro invoked from a different package fails; from another file
  in the same package it succeeds.
- Two `core` modules exposing the same macro name must produce
  `AmbiguousPreludeMacro` naming both modules, never a silent last-wins.
- A macro whose body invokes a macro not visible at the call site must produce
  `UnknownMacro` naming the inner macro — the diagnostic must not imply the
  outer macro is undefined.
- `println$()` with zero arguments must expand to a bare newline, not a parse
  error (the variadic accepts zero arguments; `docs/12-macros.md:56-59`).

### Regression risk

- **`compiler/omega-parser/tests/macros.rs`'s six existing tests**, especially
  `expands_in_all_three_positions`, which asserts an exact
  `module.nodes.len() == 3` — the assertion most sensitive to any change in what
  `expand` leaves behind. All six must pass unmodified apart from the helper's
  new argument.
- **`just build-exe` on `examples/dev/main.omg`** — ~1500 lines and the de facto
  integration test for the whole language. It must keep building and running
  identically at every step. It is also the regression check for Step 3's
  parse-path restructuring and Step 4's import-arithmetic refactor.
- **`just build-core`** at every step of Part B, and **`just build-plat`** after
  Step 13.
- **`core::numerics`' three macros** must keep working with no source edit —
  the proof that hidden-by-default preserves today's behaviour exactly.
- **`cargo test`** for the whole workspace after Part A.
- Adding `Display` to `StrOps` and to the numeric `for`-blocks changes specs
  that every existing consumer of `.hash()`/`.equals()` already touches; watch
  for the cross-package extension-method visibility bug
  (`docs/14-known-issues.md:96-107`) reappearing through a `fmt` body — which is
  exactly why no `fmt` body may call a sibling extension method.

### Target coverage

- **Hosted (libc, x86-64 Linux)** — the only target any recipe builds today;
  everything above runs here.
- **Freestanding / no-allocator** — not buildable today (no second platform
  package exists), but the design must stay honest about it: verify by
  inspection that `core::io` and `core::fmt` contain no allocation, no global
  state, and no reference to `core::glue::GlobalAllocator`, so that a program
  using only `core`-level I/O links with the three console gaps and no allocator
  glue at all. `std::io` is the only part that allocates, and it is a separate
  package a freestanding target simply does not register.
