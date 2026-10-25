# Mutability and pointer syntax: `^`, `*`, and postfix `.*`

## Task Description

- **Deliverable:** Omega's mutability and pointer syntax is replaced everywhere (compiler, runtime, checks, playground, tests, Rust test snippets, docs), and the old spellings no longer parse:

  | Meaning | Old | New |
  |---|---|---|
  | mutable binding (local, global, `for`-in, foreign) | `mut x := 1` / `mut x : T` | `^x := 1` / `^x : T` |
  | mutable parameter (**new feature**) | — | `f(^a : A)` |
  | read-only pointer type | `*T` | `*T` (unchanged) |
  | mutable pointer type | `*mut T`, `*mut []T`, `*mut str`, `*mut spec S` | `^T`, `^[]T`, `^str`, `^spec S` |
  | take a read-only address | `&x`, `&arr[a..<b]`, `&[1, 2]` | `*x`, `*arr[a..<b]`, `*[1, 2]` |
  | take a mutable address | `&mut x` | `^x` |
  | dereference (read or write) | `*p`, `*p = v`, `(*p).f` | `p.*`, `p.* = v`, `p.*.f` |
  | receivers | `self`, `mut self`, `*self`, `*mut self` | `self`, *(removed)*, `*self`, `^self` |
  | annotation list value | `&[...]` | `*[...]` |

  Binary `&`, `&=`, `^`, `^=`, `*`, `*=` keep their current meanings. After the change `mut` is an ordinary identifier with no special meaning anywhere.

- **Purpose:** This is part of an upcoming breaking redesign. Symbols are reserved syntax and words are names, so mutability is `^` rather than a keyword. Each symbol means one thing: `^` is mutable, `*` is read-only. An address-taking expression is spelled like the type it produces. Dereference becomes a postfix "place" step, so paths chain left to right.

- **Chosen direction:**
  - *Prefix builds, postfix reaches in.* `*x` and `^x` are prefix unary operators that produce `*T` and `^T`. `.*` is a postfix projection at the same tier as `.field`, `[i]`, calls, and `?`. Because postfix binds tighter than prefix, `*a.b.c` is `*(a.b.c)` and `a.b.*.c` navigates through a pointer.
  - *Position decides meaning.* Writing through a pointer is still `p.* = v`. Writability comes from the pointer's type (`^T`), never from the deref spelling, so `.^` does not exist.
  - *Receivers mirror the address the call site takes* (decided by the user): `*self` is `self : *Self`, and `^self` is `self : ^Self`. The by-value mutable receiver `mut self` is removed; a body that needs a mutable copy writes `^copy := self;`. This is the one place where `^name` in a parameter list does not mean "mutable binding". The grammar keeps the contextual `self` exception, and the docs must say so explicitly.
  - `SelfMode::MutValue` is deleted, not respelled. Binding mutability no longer leaks into function-type identity (`type_key.rs` `'V'`).
  - Mutable parameters reuse the existing HIR desugaring that `mut self` uses today (`prepend_mut_self_shadow` in `compiler/omega-hir/src/lower/item.rs`): `^a : A` lowers to an immutable parameter plus a prepended `^a := a;` shadow. Parameter mutability is a property of the body's local, not of the function type. The analyzer, MIR, and codegen stay closed.
  - `.*` is a single lexer token (`DotStar`). That makes `p.*= v` lex as `p.* = v` and `p.**= v` as `p.* *= v`, rather than `.` followed by `*=`.
  - **Migration is one exact AST-driven pass**, from old syntax straight to new, by a throwaway tool built against the *pre-change* parser (see step 8). This matters because `*x` silently changes meaning from dereference to address-of: no text-level heuristic may rewrite `*` or `&`.
  - Rationale is documented once, in a new non-normative chapter `docs/language/syntax-principles.md` (step 10).

- **Rejected alternatives:**
  - `^self` as a mutable by-value receiver, or explicit `self : ^Self`-only receivers. Rejected by the user in favour of pointer shorthand.
  - Staging with both syntaxes accepted. Prefix `*` cannot mean both deref and address-of, so a coexistence phase only defers the dangerous flip and adds throwaway parser code.
  - Regex migration of `*expr` / `&expr`. It cannot tell prefix `&` from binary `&` (`a &b`), and it cannot find deref operands. A wrong rewrite can still type-check (for example `a &b` becoming `a *b`).
  - Committing the migration tool or keeping a "legacy syntax" mode in the parser. The tool is scratch-only.
  - A `.^` mutable dereference. It contradicts "writability comes from the pointer type".

## Technical Details

- **Initial context boundary:** `compiler/omega-parser` (lexer, `parser/expression.rs`, `parser/type.rs`, `parser/mod.rs`, `parser/statement.rs`, `parser/item/`, `ast/`). `compiler/omega-hir/src/lower/item.rs`. In `omega-analyzer`, only the files listed below. `omega-mangle/src/display.rs`. Docs: `docs/language/grammar.md`, `bindings-and-mutability.md`, `functions.md`, `control-flow-and-operators.md`, `docs/guide/quick-reference.md`. Read `docs/architecture/parsing-and-hir.md` only if the HIR shadow desugaring needs explaining there.

- **Affected files/symbols (verified):**
  - Lexer `compiler/omega-parser/src/lexer.rs`: `FIXED_TOKENS` is ordered longest-first. Add `punctuation(".*", TokenKind::DotStar)` after the `..`-family entries and before `.`. `spelling_tests.rs` may enumerate punctuation.
  - `parser/expression.rs::parse_unary`: in `Prefix`, `Star` becomes `AddressOf { mutable: false }` and `Caret` becomes `AddressOf { mutable: true }`. Delete `Prefix::Deref` and the `Amp` arms. `parse_postfix_loop`: `DotStar` builds `Expression::Deref`. The expression-start token set (around line 455) replaces `Amp` with `Caret`. The AST node names `DerefExpr` / `AddressOfExpr` and the HIR nodes are unchanged.
  - `parser/item/annotations.rs::parse_annotation_expr_kind`: the list form `TokenKind::Amp` + `[` becomes `TokenKind::Star` + `[`.
  - `parser/type.rs`: `parse_type` gains a `TokenKind::Caret` arm that builds `Type::Pointer(inner, true)`. `parse_pointer_type` no longer eats `mut`. `reject_function_type_receiver` recognizes the spellings `self`, `*self`, `^self`.
  - `parser/mod.rs`: `binding_prefix_here` / `BindingPrefix` use `^` instead of contextual `mut` (order: `[comp] [^] name`, so `comp ^x := ...` still parses and is rejected by the analyzer's existing `MutCompBinding`). `parse_self_param` treats `^` followed by `self` as `MutPointer`; `^` followed by any other name is a mutable parameter. `parse_param_decls` / `parse_param_decl` accept a leading `^`; their `declared_name_at` lookaheads must allow for it.
  - `parser/statement.rs`: `is_for_in_lookahead` / `parse_for_in` (`for ^x in`). `parser/item/foreign.rs::parse_optional_mut` (`foreign ^x : T;`, foreign-block entries). `ForeignMutOnFunction` is respelled. `parser/item/functions.rs` / `parser/item.rs` for global bindings. Delete `parser/contextual.rs::MUT` once it has no users.
  - `ast/type.rs::Param` gains `mutable: bool`. `ast/self_mode.rs`: delete `MutValue`; check `is_mutable` users.
  - HIR `lower/item.rs`: generalize `prepend_mut_self_shadow` / `self_shadow_stmt` into "prepend a mutable shadow for each `^` parameter" (the receiver can no longer be by-value mutable). The shadow keeps the parameter's span so that diagnostics, including the "declared mutable but never reassigned" warning, point at the parameter.
  - Analyzer: `type_key.rs::write_function` (drop `'V'`). `analysis/specs.rs` around line 405 (`by_value` check). `resolved_type.rs` around lines 943 and 1065–1142 (receiver names and type `Display`: `*mut ` → `^`, including `[?]`, `[]`, `str`, `spec`). `core_contract.rs:166`. `error/mod.rs:70`.
  - User-facing diagnostic text that names old syntax: `error/render.rs` (around lines 183–219, 577–588, 752, 793, 1123), `error/warning.rs:76-77`, `error/kind.rs`, and `omega-parser/src/diagnostics.rs`. Find the rest with `` git grep -nE '`&|`\*mut|`mut|"\*mut|mut self' -- compiler ':!*tests*' ``.
  - Demangled display `compiler/omega-mangle/src/display.rs` (lines 138–232): `*mut` → `^`. The mangled **encoding** does not change.
  - Reflection type-name strings come from the type display, so they change observably (`tests/t51_reflection/expected.stdout`, `docs/language/reflection.md`).

- **Interfaces/invariants:**
  - AST/HIR/checked/MIR representations of pointers, places, address-of, and deref stay as they are. Only `SelfMode::MutValue` disappears and `Param.mutable` is added. Codegen, ABI, and mangling encodings are untouched.
  - A mutable parameter changes neither the function type nor the overload identity nor the symbol. It costs exactly a local copy, the same as the old `mut self`.
  - `^` on a parameter is allowed only where the declaration has a body. It is a parse error in function types (`(^a : A) => R`), in foreign function declarations, and in any bodiless spec/function declaration. Verify which declaration kinds can be bodiless.
  - Spec receivers stay pointer-only (`*self` / `^self`).
  - Diagnostics follow the project standard: structured, precise, and no wrong hints. Add dedicated parse errors for:
    - prefix `&` / `&mut` (habitual C/Rust spelling): suggest `*x` or `^x`;
    - `mut` directly before a binding name or `self`, or a pointer type spelled `*mut T`: suggest `^`. These spots are otherwise ident–ident juxtapositions that can never be valid, so `mut` stays an ordinary identifier;
    - `.^`: "dereference is always `.*`; writability comes from the pointer type".

    Do not try to diagnose an old deref `*p` that now type-checks as address-of. Ordinary type errors cover it.

- **Out of scope:** pointer arithmetic, `hidden`, refinement/aliasing rules (only their spelling in docs changes), any new place semantics, `docs/plan/` (cold storage: never migrate it), and auto-memory files.

- **Risks/open questions (stop and escalate):**
  - Any construct where `^` prefix, `*` prefix, or `DotStar` creates a real grammar ambiguity not covered above, for example inside macro fragment matching.
  - Any migrated file whose scrubbed AST differs from the original for a reason other than `Param.mutable` / `SelfMode`.
  - Any `mut self` site whose rewrite to `self` + `^copy := self;` would change behavior (only 2 exist: `tests/t03_bindings_and_mutability`, `tests/t30b_function_type_receiver_errors`).

## Implementation Plan

Steps 1–7 change the compiler. Until step 9 finishes, the runtime and conformance suite will not build, which is expected. Keep each crate's own Rust tests for the code being changed passing as you go: parser tests with the parser, HIR tests with the HIR.

0. **Baseline.** `git worktree add <scratch>/omega-old HEAD` and build its `omgc`. Optionally capture `--emit=ir` for the runtime packages and every passing `tests/` case. Step 9 uses this.
1. **Lexer:** the `DotStar` token, its `describe()`, and lexer tests (`p.*=v`, `p.**=v`, `a...*b`, `x.*.*`).
2. **Expressions:** the `parse_unary` / `parse_postfix_loop` / expression-start-set / annotation-list changes. Legacy `&`, `&mut`, and `.^` diagnostics.
3. **Types:** the `^T` arm. Remove `*mut`, with the legacy `*mut` diagnostic.
4. **Bindings and parameters:** the `^` binding prefix for locals, globals, `for`-in, and foreign items; `comp ^x`; `Param.mutable`; rejection of `^` on bodiless/function-type params; remove `contextual::MUT`; the legacy `mut` diagnostic.
5. **Receivers:** parse `self` / `*self` / `^self`, delete `SelfMode::MutValue`, and fix its users (`type_key.rs`, `specs.rs`, `resolved_type.rs`, `core_contract.rs`, `reject_function_type_receiver`).
6. **HIR:** generalize the shadow desugaring to `^` parameters. Add a HIR lowering test in `compiler/omega-hir/tests/lowering.rs`.
7. **Display and diagnostic text:** the type `Display`, receiver names, mangle display, and every message/help/note that spells old syntax.
8. **Migration tool (scratch only, never committed).** A small Rust binary with a path dependency on `<scratch>/omega-old/compiler/omega-parser`, so it parses with the **old** grammar and emits **new** text from exact spans:
   - `AddressOf` nodes: replace the leading `&` with `*`, or `&mut` plus whitespace with `^`.
   - `Deref` nodes: delete the leading `*` and insert `.*` after the base's span. Parenthesize the base when it is not a primary/postfix expression (prefix unary or cast, e.g. `*<*i32>q` → `(<*i32>q).*`). Nested derefs compose (`**p` → `p.*.*`). The AST keeps no paren node, so drop parentheses that only applied the old deref before a postfix step (`(*p).x` → `p.*.x`) by checking the source text around the span.
   - Token-level rules (comments and strings are untouched because only tokens are rewritten): Ident `mut` followed by a name → `^` (`mut comp x` → `comp ^x`); `*` + Ident `mut` → `^` (types and `*mut self`); annotation `&[` → `*[`.
   - Macro bodies are token pieces (`MacroBodyPiece`), so only the token-level rules apply there. The tool must **list** every `&`/`*` token inside a macro body for hand migration (macros live in `runtime/core/*.omg`, `runtime/plat/arch/*/atomic.omg`, `runtime/std/*.omg`).
   - `mut self` (by-value) sites: report them for hand rewriting.
   - Hosts: whole `.omg` files; fenced ```` ```omega ```` blocks in `.md`; and string literals in Rust test files that parse as an Omega module with the old parser. Report every candidate that does not parse, for example intentionally malformed parser tests, for hand migration.
   - **Equivalence check per rewritten unit:** dump the old-parser AST of the original and the new-parser AST of the rewrite with `{:#?}`, scrub span/origin fields, and diff. The only allowed differences are `Param.mutable` and `SelfMode`.
9. **Run the migration in bounded batches**, verifying each before the next: `runtime/core` → `runtime/plat` → `runtime/std` → `checks/` + `playground/` → `tests/` (a few dozen cases per batch) → Rust tests, one crate at a time → docs fenced blocks. Then migrate by hand:
   - macro bodies;
   - the 2 `mut self` sites;
   - non-parsing snippets;
   - prose and comments that mention old syntax.

   Completeness greps outside `docs/plan/`, all of which must come back empty or only binary `&`:

   ```
   git grep -nE '&mut|\*mut|\bmut (self|[a-z_]+ *:)' -- '*.omg' '*.md' ':!docs/plan'
   ```

   and a review of prefix `&` hits. Regenerate `expected.stderr`/`expected.stdout` only where the diff is a pure spelling change (`*mut T` → `^T`, message text). Investigate any other change. If step 0 captured IR, rebuild it with the new compiler and diff. Differences may only be source-location constants (columns shift) and the `mut self` sites.
10. **Docs** (update the owning layer, spelling and semantics only):
    - `docs/language/grammar.md`: productions at lines 40–41, 134, 143, 150, 191, 203, 228, 247–249, 423, 449–450, 474, 522–524; the precedence table (unary `- ! * ^ ~ ++ -- reveal comp`; postfix adds `.*`); the receiver exception for `^self`.
    - `bindings-and-mutability.md` (binding forms, the "what may be mutable" list now including parameters, receiver forms, mutable places).
    - `functions.md` (mutable parameters and the bodiless-declaration rule).
    - `control-flow-and-operators.md` (address-of and deref operators, `.*` as a place).
    - `lexical-structure.md` (the `.*` token; `mut` no longer contextual).
    - `reflection.md` (type names).
    - Every other `docs/language/` and `docs/guide/` chapter with old spellings.
    - `docs/architecture/` (`symbol-mangling.md` display, `parsing-and-hir.md` if it describes the `mut self` shadow).
    - `docs/issues/` (spelling only).
    - New `docs/language/syntax-principles.md`, marked **non-normative rationale**, linked from `docs/language/README.md`, from the grammar precedence section, and from `bindings-and-mutability.md`. It records the user's seven principles:
      1. symbols over keywords;
      2. one symbol, one meaning;
      3. expressions spelled like their types, and why C's "declaration mirrors use" doesn't apply to prefix types;
      4. prefix builds, postfix reaches in, and why C needed `->`;
      5. precedence follows from it (`*a.b.c`);
      6. a dot means a place, while bare postfix `?` is control flow;
      7. position decides meaning, so dereference is always `.*` and writes are `p.* = v`, never `.^`.

      Also record the receiver exception (`^self` = `self : ^Self`) and why the by-value mutable receiver was removed.

## Testing

- **New/changed cases:**
  - Parser unit tests (`compiler/omega-parser/src/parser/expression/tests.rs` and the type/statement test modules) cover:
    - prefix `*` / `^` precedence against postfix (`*a.b.c`, `^a[i]`, `*arr[0..<n]`);
    - `p.*.f`, `p.*.*`, `p.*= v`;
    - binary `a & b`, `a ^ b`, `a * b.*`;
    - each legacy-syntax diagnostic.
  - HIR lowering test for the `^` parameter shadow.
  - New conformance case `tests/t03c_pointer_syntax/` (runtime-checked `expected.stdout`). It proves:
    - `*x : *T`, `^x : ^T`;
    - reads and writes through `p.*`, field paths through pointers, slices via `*arr[a..<b]` / `^arr[a..<b]`;
    - `*[...]` compile-time slices;
    - a `^a : A` parameter mutated inside the callee leaving the caller's value unchanged;
    - `for ^x in`, a `^` global, and `^self` / `*self` receivers with call-site auto-adapt.
  - Update `tests/t03_bindings_and_mutability` (its `mut self` case becomes `self` + `^copy := self;`) and `tests/t30b_function_type_receiver_errors` (drop the `mut self` alias; cover `^self`).
- **Specification trace:**
  - `docs/language/bindings-and-mutability.md` (binding forms, receivers, mutable places);
  - `functions.md` (mutable parameters);
  - `control-flow-and-operators.md` / `grammar.md` (operators and precedence);
  - `strings-casts-arrays-and-slices.md` (slice address-taking).
- **Negative/diagnostic cases:** new `tests/t03d_pointer_syntax_errors/` with an exact `expected.stderr` covering:
  - `&x`, `&mut x`, `mut x := 1`, `x : *mut i32`, `p.^`;
  - `p.* = v` through a `*T`;
  - assigning an immutable parameter;
  - `^` on a function-type parameter and on a foreign function parameter;
  - `comp ^x := 1` (`MutCompBinding`);
  - `^x` of an immutable binding.

  Check every diagnostic's text, not just that compilation fails.
- **Regression coverage:** the full conformance suite (it exercises every migrated case), `t51_reflection` (type names), and `omega-mangle/tests/roundtrip.rs` (display). Also the analyzer and driver Rust test suites (`omega-driver/tests/conform.rs`, `aliases.rs`, `generic_methods.rs`, `diagnostics.rs`), because they embed many Omega snippets.
- **Commands/target coverage:** `cargo test -p omega-parser`, `-p omega-hir` while working, then `cargo test --workspace`. Run `./bin/test-runner t03c_pointer_syntax t03d_pointer_syntax_errors t03_bindings_and_mutability t30b_function_type_receiver_errors t51_reflection`, then `just test-all`. Also build the runtime for the non-host arch targets touched by the macro migration (`aarch64`, `avr`) through the `justfile` `build-runtime-for` recipe, and run the `checks/` packages through their existing recipes.
