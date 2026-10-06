# `config$`: compiler configuration as values

## Task Description

- **Deliverable:**
  1. Unary `-` applied directly to a numeric literal is range-checked as one signed value in every expression, so `-128i8` and `-2147483648` are valid source.
  2. A new compiler-implemented core macro `config$(x)` evaluates `x` with the `@cond` evaluator and expands to one literal.
  3. A new condition operator `default(def::name, literal)`.
  4. Unsuffixed `-D` numbers become adaptable, in both `@cond` and `config$`.
- **Purpose:** today configuration only selects declarations. Valued definitions (`-DBUF=4096`) can never be used as values, picking a value takes duplicate declarations guarded by `@cond`, and function bodies cannot branch on configuration. Target builtins are also wanted as values, e.g. a program that prints its OS.
- **Chosen direction (settled with the user, do not reopen):**
  - `config$(x)` is declared in `runtime/core/builtins.omg` beside `file$`/`line$`/`column$`.
  - Its single `expr` argument is reparsed as `@cond` condition syntax and evaluated by `omega_analyzer::annotation_eval` at expansion time. It is never resolved, type-checked or `comp`-evaluated as Omega.
  - Results:
    - a definition or `default(...)` expands to the literal as written, so an unsuffixed literal stays unsuffixed and adaptable;
    - a builtin expands to a literal of its declared type (`target_pointer_width` becomes a `u32`-suffixed literal; the others are strings or a bool);
    - an operator call expands to `true`/`false`.
  - A negative number expands as `(`, `-`, number, `)`. The parentheses keep postfix use such as `config$(def::x).abs()` correct. Step 1 makes that form valid for signed minimums.
  - `default(def::name, literal)` yields the definition's value if one was supplied, otherwise the literal. It is valid wherever a condition value is: in `@cond` and in `config$`.
- **Rejected alternatives:**
  - Exposing `def::` to name resolution, or a `core::config` function: contradicts the spec, and would create a comp-only class of function.
  - One macro per builtin, or `def$`/`def_or$`: too much surface.
  - A compiler-only sign-carrying literal token: a literal form source cannot write. Fixing negation globally (step 1) replaces it.
  - Typed `i32`/`f32` definitions: friction for `usize` uses, and the `f32` rounding trap (`equals(def::ratio, 0.1f64)` would be false).

## Technical Details

- **Initial context boundary:**
  - `compiler/omega-analyzer/src/annotation_eval.rs` and `compiler_definitions.rs`;
  - `compiler/omega-parser/src/macros.rs`, `macros/expander.rs`, `ast/item.rs` (`MacroBuiltin`), `parser/item/annotations.rs`;
  - `compiler/omega-driver/src/modules.rs` (the `expand_with_origins` call around line 677);
  - `compiler/omgc/src/cli.rs` (`-D` decoding around line 185);
  - for step 1: `compiler/omega-analyzer/src/analysis/exprs/operators.rs` (`analyze_negate`), `analysis/literals.rs` (`analyze_number`, `parse_number_literal`), `analysis/consts.rs` (`const_number`);
  - docs: `docs/language/annotations-and-sizeof.md` (`@cond` section), `docs/language/macros.md`.

- **Affected files and symbols:**
  - **Negation (step 1).** `analyze_negate`, or its dispatch at `analysis/exprs/mod.rs:76`, special-cases a base of `HirExpr::Number` and decodes sign and magnitude together. Parentheses produce no AST/HIR node, so `(-128i8)` reaches the same path. `consts.rs::const_number(.., negated)` already does this for enum values and constant slices. Share one signed decoder with it rather than adding a second one; `parse_number_literal` currently takes no sign. Unsigned targets still reject negation (`InvalidNegateOperand`), and floats are unchanged.
  - **Definition storage.** `CompilerDefinitions` stores each user value as its written `AnnotationLiteral`, so `-Dflag` becomes `AnnotationLiteral::Bool(true)`. It is decoded at each use through `Evaluator::literal(.., context)`, exactly like a literal written in a condition. `established_kind` for `def::x` returns only a *declared* suffix kind, never a default. `DefinitionValue` remains the evaluator's decoded working type. `omgc/src/cli.rs` still parses with `parse_literal` and rejects malformed literals, unknown suffixes, and suffixed values out of range for their suffix. Range checks for unsuffixed numbers move to the point of use.
  - **The `default` operator.** Add it to `Evaluator::call`, together with its value and boolean-position handling and its contribution to `established_kind`. Add `ConditionErrorKind` variants for: a first operand that is not `def::name`, a second operand that is not a literal, and mismatched kinds. The kind check runs even when the definition is supplied.
  - **Value entry point.** Add a public `annotation_eval` function that evaluates one `AnnotationExpr` in value position and returns a parser-owned `AnnotationLiteral`. It follows the evaluation order and error rules of `evaluate`, including evaluating every operand, and reports an absent bare `def::x` as `MissingDefinition`.
  - **Expander hook.** The expander currently receives only `ItemFilter`. Replace it with one small parser-owned configuration interface carrying both operations: keep or drop an item, and evaluate a `config$` argument. Both share the caller's error type `E`. Rename `ExpansionFailure::Filter` if it no longer reads correctly. The template-only `macros::expand` path keeps every item and fails `config$` the way `BuiltinWithoutSourceContext` does. The driver implements the interface with `item_is_enabled` and the new value function.
  - **`MacroBuiltin::Config`.** `bind_definition`'s shape check becomes per-builtin: `config` requires exactly one fixed `expr` parameter and no variadic; the others keep zero parameters. In `Expander` (around `builtin_token`), `Config` reparses its argument tokens with the annotation-expression parser. Add a `pub(crate)` entry in `parser/item/annotations.rs` that requires all tokens to be consumed. The expander then calls the hook and converts the returned `AnnotationLiteral` into tokens carrying the invocation's span and origin. Macro invocations inside the argument are not pre-expanded, so they fail to parse as a condition. That error must say that a condition cannot invoke a macro.

- **Interfaces and invariants:**
  - **Acyclic order.** `config$` reads only `CompilerDefinitions`. Nothing it produces is visible to `@cond` filtering.
  - **No silent false outside boolean positions.** The top level of `config$` is a value position, so an absent bare definition is an error.
  - **Same restrictions as `@cond`.** The argument cannot be ordinary Omega (`1 + 2`, locals, `comp` bindings). A user macro that substitutes `$e` into `config$($e)` works, because rendering happens before the builtin expands.
  - **Builtins** describe the selected target and keep their declared types.
  - **The `@cond` behavior change is visible only for floats** and for the range of unsuffixed definitions: integers already compare by mathematical value.
  - **Separate compilation.** The existing "compatible definitions" rule covers value use. Nothing new is recorded in artifacts.

- **Out of scope:** new builtins, a configuration manifest or schema, recording configuration in artifacts, `sizeof` in conditions, and any other literal-decoding changes.

- **Risks to stop and escalate on:**
  - The configuration interface cannot be threaded through the expander without changing generated-item filtering order.
  - Folding negation changes behavior beyond accepting signed minimums: different types, a changed diagnostic for `-x` on non-literals, or a changed `-` on unsigned literals.

## Implementation Plan

1. **Negative literals.**
   - Implement the shared signed literal decoder and use it in `analyze_negate` for a `HirExpr::Number` base and in `const_number`.
   - Update `docs/language/types-and-primitives.md` ("Numeric literal inference"): unary `-` applied directly to a numeric literal, including through parentheses, is range-checked as one negative value.
   - Delete the "signed minimum" entry from `docs/issues/language-limitations.md`.
2. **Adaptable definitions.**
   - Store user definitions as written literals; update `user()`, `established_kind`, value/boolean evaluation, and `omgc` decoding.
   - Update `annotation_eval` and `compiler_definitions` unit tests and `compiler/omgc/tests/compiler_definitions.rs`.
   - Replace "A supplied definition keeps the type it was defined with" in `annotations-and-sizeof.md`.
3. **The `default` operator**, with evaluator unit tests and its row in the `@cond` operators table.
4. **The value entry point** in `annotation_eval`, with unit tests.
5. **Expander configuration interface.** Thread it through `macros.rs`, `expander.rs` and the driver (`modules.rs`). There is no behavior change yet, and existing tests must still pass.
6. **`MacroBuiltin::Config`**: declaration, shape check, argument reparse, and token conversion. Add the declaration to `runtime/core/builtins.omg` and adjust that file's header comment, which currently says every builtin describes the invocation site. Add parser tests in `compiler/omega-parser/tests/builtin_macros.rs`.
7. **Documentation.**
   - `annotations-and-sizeof.md` owns the `config$` rules in a new subsection of the `@cond` section: argument, results, position rules, adaptability, and the negative expansion. Also amend the sentence "not visible to any expression": definitions are still outside name resolution, and `config$` is the only way to read them.
   - `macros.md` "Compiler-implemented core macros" lists `config$` in one line with a link.
   - In `docs/guide/compiler-cli.md`, the definitions section mentions `config$` alongside `@cond`.
   - `docs/guide/quick-reference.md` gets a short `config$`/`default` example next to the `@cond` one.

## Testing

- **New root conformance cases**, with `compiler.definitions` where needed (see `docs/architecture/testing-and-validation.md`):
  - **new `t49_negative_literals`:** `-128i8`, `-2147483648` in an `i32` position, `(-128i8)`, and a negated literal adapting to an `i8` expected type all compile and print correctly. Trace: the types-and-primitives rule from step 1.
  - **`t45o_config_values`:**
    - an unsuffixed definition used as `usize`, `u8` and `i64`;
    - a suffixed definition keeping its type;
    - `-Dlow=-128i8` and an unsuffixed signed minimum;
    - each builtin printed (the OS-printing example);
    - an operator call as a `bool` inside `if`;
    - `default` with both a supplied and an absent definition;
    - `config$` through a wrapping user macro;
    - `config$(def::x).method()` postfix use.

    Trace: the new `config$` subsection.
  - **`t45p_conditional_default`:** `default` inside `@cond`, e.g. `greater(default(def::BUF, 0), 1024)`; and an unsuffixed float definition equal to a `f64` literal (adaptability in `@cond`).
- **Negative and diagnostic cases.** Each checks `expected.stderr`, not just a failed exit:
  - `config$(def::missing)`: missing-definition error.
  - `config$(1 + 2)` and `config$(LOCAL)`: not a condition.
  - `config$(other$())`: a condition cannot invoke a macro.
  - `default(target_os, "x")`, and `default(def::n, "x")` with `-Dn=5`: mismatched kinds.
  - An unsuffixed `-Dsmall=300` used as `u8`: an ordinary out-of-range literal error at the use site.
  - A malformed `core::builtins` `config` declaration: a Rust parser test.
- **Regression coverage:**
  - all `tests/t45*` cases, `t15_annotations_and_sizeof`, `t01_lexical_structure`, `t18_macros`;
  - `cargo test -p omega-analyzer annotation_eval compiler_definitions`;
  - `cargo test -p omega-parser --test builtin_macros --test conditional_compilation --test macros`;
  - `cargo test -p omgc --test compiler_definitions`;
  - enum-value and constant-slice tests that exercise `const_number`.
- **Commands:**
  - focused: `./bin/test-runner t45o_config_values t45p_conditional_default t49_negative_literals t45b_conditional_definitions`;
  - final gate: `just test-all`.
