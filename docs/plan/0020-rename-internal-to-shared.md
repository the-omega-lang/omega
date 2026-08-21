# Rename the `internal` visibility keyword to `shared`

## Task Description

- **Deliverable:** the Omega contextual keyword currently spelled `internal`
  (package-wide declaration visibility) is spelled `shared` everywhere it is
  a language keyword: lexer/parser keyword table, the `Visibility` enum
  variant and its `Display` impl, every diagnostic/help string that names the
  modifier, `docs/language/` and `docs/guide/` prose/examples, the relevant
  `docs/architecture/` sentences that describe this specific visibility
  level, and every `.omg` source file under `runtime/` and `tests/` that
  currently writes `internal` as a modifier. Compiler/test builds and
  `just test-all` are green afterward.
- **Purpose:** `internal extern name : T;` reads as two clashing ideas
  (`extern` = "from outside", `internal` = "not from outside") in the same
  phrase. `shared` keeps the same package-wide scope but reads correctly
  next to `extern`, is shorter, and carries no competing pre-existing
  meaning in the language the way `extern` does.
- **Chosen direction:** a straight rename of one visibility level's spelling,
  keyword-by-keyword and site-by-site, not a search-and-replace of the
  substring `internal`. The Rust-side identifier that models this exact
  concept (`Visibility::Internal`) is renamed to `Visibility::Shared` in the
  same pass, since it is the direct 1:1 representation of the keyword being
  renamed, not an unrelated "implementation-internal" concept.
- **Rejected alternatives:** renaming `extern` instead (user's call, already
  decided — `extern` is the more established/loaded term and touches a much
  larger surface, e.g. all FFI docs); a global textual find-and-replace of
  `internal` (would corrupt unrelated English usages like "internal compiler
  bug", "internal HashMap iteration order", "internal ABI/calling
  convention" — see the "Do NOT touch" list below, which was verified by
  reading every match, not guessed).

## Technical Details

### Initial context boundary

- `compiler/omega-parser/src/parser/contextual.rs`, `ast/visibility.rs`,
  `parser/item.rs`, `parser/item/definitions.rs`, `diagnostics.rs`.
- `compiler/omega-analyzer/src/analysis/visibility.rs`,
  `error/render.rs`.
- `compiler/omega-driver/src/modules.rs`, `src/items/resolution.rs`.
- `docs/language/visibility.md` (normative chapter for this keyword),
  `docs/language/grammar.md`, `docs/language/lexical-structure.md`,
  `docs/language/macros.md`, `docs/language/structs-and-unions.md`,
  `docs/language/foreign-function-interface.md`, `docs/language/README.md`.
- `docs/guide/quick-reference.md`, `docs/guide/platform-glue.md`.
- `docs/issues/design-debt.md`, `docs/issues/language-limitations.md`.
- `docs/architecture/semantic-analysis.md` (two sentences only).
- All `.omg` files under `runtime/` and `tests/` (list verified below).

### Affected files/symbols — rename the keyword here

Rust/keyword-table sites (verified by reading each, not by blind grep):

- `compiler/omega-parser/src/parser/contextual.rs`: constant `INTERNAL`
  (rename to e.g. `SHARED`, value `"shared"`); update its entry in the `ALL`
  slice.
- `compiler/omega-parser/src/ast/visibility.rs`: enum variant
  `Visibility::Internal` -> `Visibility::Shared`; its `Display` arm
  `"internal"` -> `"shared"`.
- `compiler/omega-parser/src/parser/item.rs` (~line 205-210): the
  `contextual::INTERNAL` match arm and the `Visibility::Internal` it
  constructs.
- `compiler/omega-parser/src/parser/item/definitions.rs` (~line 121-125):
  the `field_follows` disambiguation comment and the
  `contextual::EXPOSED || contextual::INTERNAL` check (rename the constant
  reference; update the comment's `` `exposed`/`internal` `` mention).
- `compiler/omega-parser/src/diagnostics.rs` (~line 90): help text
  `` "'exposed'/'internal' are only allowed on..." `` -> `'exposed'/'shared'`.
- `compiler/omega-analyzer/src/analysis/visibility.rs`: two
  `Visibility::Internal => ...` match arms (`check_visibility`'s
  `visibility_allows` and its sibling); no string literals here.
- `compiler/omega-analyzer/src/error/render.rs`: two help strings
  `` "mark the field `exposed`/`internal` on ..." `` and the method
  equivalent -> `` `exposed`/`shared` ``.
- `compiler/omega-driver/src/modules.rs` (~line 307):
  `definition.visibility == Visibility::Internal`.
- `compiler/omega-driver/src/items/resolution.rs` (~line 11):
  `Visibility::Internal => declaring.first() == accessor.first()`.

Rust test sites:

- `compiler/omega-parser/src/tests.rs` (~line 42, 44): two `.omg` source
  literals `"internal gap Foo { ... }"` / `"internal glue Foo { ... }"` used
  to prove gaps/glues reject a visibility modifier -> `"shared gap Foo ..."`
  / `"shared glue Foo ..."`.
- `compiler/omega-parser/tests/macros.rs` (~line 71-73): source literal
  `"internal macro make() => {}"` and the assertion
  `internal.visibility == Visibility::Internal` -> `"shared macro make() =>
  {}"` / `Visibility::Shared`. The local variable name `internal` may stay
  or be renamed to `shared` at the developer's discretion; keep it
  consistent with the literal it holds.
- `compiler/omega-driver/tests/entry_point.rs` (~line 71): source literal
  `internal extern exit : (code: i32) => never;` -> `shared extern exit :
  ...`.
- `compiler/omega-driver/tests/conform.rs` (~line 968, 982): source literal
  `"internal shared() => i32 { 42 }"` — note this test already names an
  *unrelated* function `shared`, so a literal keyword rename produces
  `"shared shared() => i32 { 42 }"`, which is syntactically valid but
  confusing to a reader. Rename the function identifier too (e.g. `fortytwo`
  or `shared_value`) at both the definition site and its two call sites in
  the same test, so the keyword and the identifier don't collide visually.
  Read the surrounding test (search `shared()` in that file) before editing
  so both the child-module definition and the call sites stay consistent.

`.omg` sources using the keyword (verified list; edit the `internal`
modifier occurrences only — do not touch unrelated content in these files):

- `runtime/core/range.omg`
- `runtime/std/io.omg`, `runtime/std/alloc.omg`, `runtime/std/hash_set.omg`,
  `runtime/std/list.omg`, `runtime/std/hash_map.omg`,
  `runtime/std/linked_list.omg`
- `runtime/plat/libc/libc.omg` (includes the `internal extern _omg_main :
  () => void;` adapter declaration added by the prior entry-point task —
  rename it too, it is an ordinary use of the same keyword)
- `tests/t01_lexical_structure/t01_lexical_structure.omg`
- `tests/t13_visibility/t13_visibility.omg`, `tests/t13_visibility/child.omg`
  (this is the conformance case for `docs/language/visibility.md` itself —
  read both files fully before editing so every `internal` occurrence,
  including any in comments explaining the test, is updated coherently)
- `tests/t18_macros/defs.omg`
- `tests/t19_foreign_function_interface/t19_foreign_function_interface.omg`
- `tests/t22_program_entry_point/t22_program_entry_point.omg` (`internal
  extern exit : ...`)

Re-run `grep -rn '\binternal\b' runtime/ tests/ --include=*.omg` after
editing to confirm the count is exactly zero; do not assume this list is
exhaustive if the tree has changed since planning.

Documentation sites (prose describing the keyword itself):

- `docs/language/visibility.md`: the whole chapter is about this keyword —
  read and update it fully (code sample, the `internal` section heading and
  body, the bullet list, the specs/conformance example).
- `docs/language/grammar.md` (~line 49): `visibility = "exposed" |
  "internal" ;` -> `"shared"`.
- `docs/language/lexical-structure.md` (~line 37): contextual-keyword list
  `mut comp self reveal sizeof in exposed internal` -> `... exposed shared`.
- `docs/language/macros.md` (~line 110): `` `macro` accepts the same
  hidden/default, `internal`, and `exposed` `` -> `` `shared` ``.
- `docs/language/structs-and-unions.md` (~line 31): `` Fields and methods
  may be hidden, `internal`, or `exposed` `` -> `` `shared` ``.
- `docs/language/foreign-function-interface.md` (~line 10, 28): two `.omg`
  code samples `internal extern malloc : ...` / `internal extern printf :
  ...` -> `shared extern ...`. Leave the *unrelated* "fixed internal symbol"
  (~line 59) and "Omega's internal calling convention" (~line 65) sentences
  untouched — those describe compiler-internal linkage/ABI facts, not this
  keyword.
- `docs/language/README.md` (~line 24): `` hidden/exposed/internal items and
  members `` -> `` hidden/exposed/shared ``. Leave line 13
  ("Compiler-internal Rust type names...") untouched — unrelated meaning.
- `docs/guide/quick-reference.md` (~line 397): example `internal
  package_api() => void { }` -> `shared package_api() => void { }`.
- `docs/guide/platform-glue.md` (~line 29, 87): `` `internal extern`
  bindings `` and `` are `internal` (package-wide, ... `` -> `shared`.
- `docs/issues/design-debt.md` (~line 198): `` A hidden/internal access ``
  -> `` A hidden/shared access ``.
- `docs/issues/language-limitations.md` (~line 139): `` `internal macro` is
  package-visible `` -> `` `shared macro` ``.
- `docs/architecture/semantic-analysis.md` (~line 117, 294): ``
  `visibility.rs` — exposed/internal/hidden/reveal checks `` and `` A
  successful hidden/internal access marks ... `` -> `shared` in both.

### Do NOT touch — verified unrelated uses of the English word "internal"

These describe implementation-internal (private-to-the-compiler) concepts,
not the Omega visibility keyword. Leave exactly as-is:

- `compiler/omega-codegen/src/llvm/mod.rs` ("internal compiler bug",
  "internal error: the LLVM backend...").
- `compiler/omega-codegen/src/cranelift/function.rs` ("internal codegen
  bug").
- `compiler/omega-analyzer/src/error/render.rs` (~line 526: "Omega's calling
  convention is internally consistent...").
- `compiler/omega-driver/src/roots.rs` ("internal HashMap iteration
  order").
- `docs/architecture/abi-and-representation.md`, `compiler-overview.md`,
  `diagnostics.md`, `mir-and-codegen.md`, `runtime-and-platform.md`,
  `symbol-mangling.md` — every "internal" there means "compiler-internal
  convention/symbol/failure", not this keyword.
- `docs/language/foreign-function-interface.md` line ~59 ("fixed internal
  symbol") and ~65 ("Omega's internal calling convention").
- `docs/issues/known-issues.md` (`<internal HirId>`).
- `docs/language/iteration-and-ranges.md` ("internal cursor").
- `docs/language/README.md` line ~13 ("Compiler-internal Rust type
  names...").

### Out of scope

- `docs/plan/*.md` — historical cold storage per `ARCHITECTURE.md`; do not
  rewrite past plan documents to match the new spelling.
- Any file not listed above where `internal`/`Internal` appears with an
  unrelated meaning. If a new occurrence turns up that isn't in either list,
  classify it yourself using the same test (does it name *this* visibility
  keyword, or an unrelated "implementation-internal" concept?) before
  touching it — do not assume every match is in scope.
- No change to `exposed`, hidden (no-modifier), or `reveal` semantics or
  spelling.
- No change to visibility *behavior* (package-wide scope rules, `reveal`
  interaction, macro visibility-smuggling rule) — this is a pure rename.

### Interfaces/invariants

- `Visibility` stays a 3-variant enum with the same ordering/derives
  (`Hidden`, then the renamed variant, then `Exposed`) — `PartialOrd`/`Ord`
  are derived from declaration order, and nothing in this rename should
  change relative ordering semantics.
- The parser's contextual-keyword mechanism (`ALL` slice in
  `contextual.rs`) must still list every contextual keyword exactly once;
  removing `INTERNAL`/adding `SHARED` must keep the list's use sites (e.g.
  wherever `ALL` is consumed for reserved-identifier checks) correct.
- `shared` must not already be used as a keyword or reserved contextual
  identifier elsewhere in the grammar (verified: it is not in
  `contextual::ALL` today).

### Risks/open questions

- None requiring escalation. The one judgment call already resolved above:
  `conform.rs`'s pre-existing function literally named `shared` gets renamed
  alongside the keyword so the two don't collide in the same source string.

## Implementation Plan

1. Rename the keyword table and AST representation first, so the compiler
   fails to build until every consumer is updated (fast feedback):
   `parser/contextual.rs` (`INTERNAL` -> `SHARED`, `"internal"` ->
   `"shared"`), `ast/visibility.rs` (`Visibility::Internal` ->
   `Visibility::Shared`, `Display` arm).
2. Fix the resulting compile errors in `parser/item.rs`,
   `parser/item/definitions.rs` (including its comment), `diagnostics.rs`'s
   help string, `analysis/visibility.rs`'s two match arms,
   `error/render.rs`'s two help strings, `driver/modules.rs`, and
   `driver/items/resolution.rs`. Run `cargo build --workspace` until clean.
3. Update the Rust test literals: `omega-parser/src/tests.rs`,
   `omega-parser/tests/macros.rs`, `omega-driver/tests/entry_point.rs`,
   `omega-driver/tests/conform.rs` (including the function-name collision
   fix described above). Run `cargo test --workspace` until clean.
4. Update every `.omg` source listed under "Affected files/symbols" in
   `runtime/` and `tests/`, one directory at a time (`runtime/core`,
   `runtime/std`, `runtime/plat/libc`, then `tests/`), re-grepping for
   `\binternal\b` after each directory to catch anything missed.
5. Update `docs/language/` chapters, then `docs/guide/`, then the two
   `docs/issues/` sentences, then the two `docs/architecture/` sentences —
   in that order, matching documentation-authority precedence.
6. Final sweep: `grep -rn '\binternal\b' compiler/ docs/language docs/guide docs/issues docs/architecture runtime/ tests/ --include=*.rs --include=*.omg --include=*.md` (excluding `docs/plan/`) and confirm every remaining hit is one of the verified "do not touch" unrelated uses.

## Testing

- **Regression coverage (primary proof):** `tests/t13_visibility/` is the
  existing conformance case for the visibility chapter; after the rename it
  must still pass with `shared` in place of `internal` and prove the same
  package-wide-access behavior (its own `expected.stdout`/`expected.stderr`,
  if any, do not need semantic changes — only the source's spelling
  changes). Also re-check `tests/t18_macros/` and
  `tests/t19_foreign_function_interface/`, which use the keyword.
- **Component tests:** `cargo test --workspace`, focusing on
  `omega-parser` (`tests.rs`, `tests/macros.rs` — prove the token `shared`
  parses into `Visibility::Shared` and that `internal` is no longer special)
  and `omega-driver` (`tests/conform.rs`, `tests/entry_point.rs`).
- **Negative check:** confirm `internal` used as a visibility modifier is no
  longer accepted specially — e.g. `internal struct Foo {}` should now parse
  `internal` as an ordinary (hidden-visibility) identifier/type-like token
  rather than a visibility modifier, per however `parse_optional_visibility`
  falls through for a non-keyword identifier. This is implied by the
  contextual-keyword mechanism already in place; no new test is required
  solely for this unless a component test doesn't already exercise it.
- **Full gate:** `just test-all` after the `.omg` source and Rust changes
  are both in place, since runtime packages (`runtime/core`, `runtime/std`,
  `runtime/plat/libc`) must still build and link with the new spelling
  before any root conformance case can run.
- **Commands:** `cargo build --workspace`, `cargo test --workspace`,
  `just test-all`.
