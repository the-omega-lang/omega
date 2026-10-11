# Nested proof patterns for `match`

## Task Description

- **Deliverable:** `match` arms can describe field shapes: `Event::Click { pos = Point { x = 0; .. }; button = Button::Left; }`. Nested patterns prove facts and refine field paths. They never bind names. A struct-typed value may be a `match` scrutinee. Arm selection follows the specificity/order rules below. The spec, guide, architecture docs and conformance tests are updated to match.
- **Purpose:** Omega already reaches variant data through refinement (`opt.value`). What it lacks is discrimination on nested shapes and field values, with exhaustiveness checking. This adds that without bindings, binding modes, or a new reference-binding kind, so there is no hidden copy or alias.
- **Chosen direction:**
  - **Patterns prove, never bind.** An arm `Option::Some { value = Result::Ok; }` proves `res` is `Some` *and* `res.value` is `Ok`. The arm reads `res.value.value`.
  - **Selection rules.** The rules compare two arms field by field, recursively:
    1. At a leaf, two arms' subpatterns must be disjoint, equal, or one inside the other. A leaf is an integral/bool/char value or range, or the variant/member chosen at an enum position. Partial overlap at a leaf is an error (`x = 0..<10` vs `x = 5..<20`).
    2. If arm A lies inside arm B on every field and the two differ, A is more specific. A must appear before B and runs on the overlap.
    3. Arms nested in opposite directions on different fields have the same weight (`Point { x = 0; .. }` vs `Point { y = 0; .. }`). They may overlap, and the earlier arm runs on the overlap.
    4. An arm fully covered by the earlier arms combined is an **error** (unreachable arm). This matches today's treatment of a redundant `..` as an error. A dead `else` block stays a warning as today.
    5. Identical patterns are an error.

    Result: the first matching arm in source order is always the one that runs, and source order always agrees with specificity.
  - **Top-level `..` is the most general arm** (user decision). It matches every declared variant/value, so rule 2 forces it last. The "remainder must be one contiguous range" restriction for value matches is **removed**: `0 => a, .. => b` becomes legal. At most one `..` (rule 5).
  - **Value matches are the single-leaf case.** Every currently valid match keeps its meaning. `5 => ..., 0..<10 => ...` becomes legal, and partial overlap stays an error.
  - **Struct scrutinees are allowed** (user decision): `match pt { Point { x = 0; .. } => ..., Point { .. } => ... }`.
  - **Field syntax mirrors construction:** `name = subpattern;`, each field terminated by `;` exactly like struct literals (`StructLiteralField`). An optional trailing `..` (no `;`) ignores the remaining fields. Without `..`, every patternable field must be listed. `Path { .. }` is the same as bare `Path` for a variant.
- **Rejected alternatives:**
  - Unnamed/positional variant fields.
  - Binding destructuring (`=` place vs `:=` copy, per-field `&field`): needs binding modes or a new reference binding.
  - Plain first-match-wins: order would silently decide arbitrary overlaps.
  - Pure most-specific-wins that rejects every same-weight overlap: the user chose order for ties.
  - Numeric specificity weights: they tie and can disagree with containment.
  - `..` keeping remainder semantics: it would be exempt from the source-order principle.

## Technical Details

- **Initial context boundary:**
  - **Docs:** `docs/language/enums-and-pattern-matching.md` (`## match`, `### Enum patterns`, `### Exhaustiveness and overlap`, `### Invalid tags`, `## Range patterns`/`### Bare .. catch-all`, `## Variant-specific access and widening`), `docs/language/bindings-and-mutability.md` (`## Mutable places`), `docs/language/grammar.md` (`### match`), and `docs/architecture/semantic-analysis.md` (`## Pattern matching and exhaustiveness`).
  - **Code:** `compiler/omega-parser`, `compiler/omega-hir` and `compiler/omega-analyzer`, limited to the sites below.
  - **MIR stays closed.** `lower_match_chain` (`compiler/omega-mir/src/lower/function/control_flow.rs`) already lowers arms as an ordered chain of OR-of-AND condition groups, which is exactly first-match semantics. If you find you need to change MIR, stop and escalate.
- **Affected files/symbols:**
  - **Parser:**
    - `ast/expression.rs`: add a field-pattern form to `PatternValue`, e.g. `Fields { path: ExprPath, fields: Vec<FieldSubpattern { name, name_span, name_origin, pattern: Pattern }>, rest: Option<Span>, span }`. Subpatterns reuse `Pattern`, which keeps both value and type readings, so anonymous-enum member types work at nested positions.
    - `parser/expression.rs` (`parse_pattern`, `probe_type_pattern`, `parse_pattern_value`): a pattern-position path followed by `{` parses as a field pattern, not a struct-literal expression. The type-reading probe must accept `;` (field subpattern) as well as `=>` (arm) as its terminator.
    - `macros/expander.rs` (~line 809): expand the new variant recursively. Export it via `prelude.rs`.
  - **HIR:** `hir.rs` (`HirPattern`, `HirPatternValue`) gets the mirrored `Fields` variant. `lower/expression.rs` (`lower_pattern`) lowers it recursively and stays syntax-only. Keep `HirPattern::catch_all_range` meaning a top-level bare `..`.
  - **Analyzer, pattern space:** generalize `crate::exhaustiveness` (currently integer `Interval`s, `CoverageReport`, `check`) into a pattern-space engine. The current interval logic becomes the integral-leaf case.
    - Node kinds: any; integral interval; enum variant with field subpatterns; anonymous member with an optional nested pattern; struct with field subpatterns.
    - Operations:
      - leaf/field relation: disjoint, equal, sub, super, same-weight (mixed), or partial-overlap error;
      - subtraction of a pattern from a remaining space, producing a cell list;
      - rendering a cell as pattern text for diagnostics.
    - Data model, so later features need no new node kinds:
      - The product node (struct and variant fields) is keyed by a generic component key that holds only field names for now. A later multi-scrutinee `match` becomes a product with positional keys.
      - An arm is a list of alternatives that always has exactly one entry for now. Checks and subtraction iterate over it. The refinement an arm grants is whatever every alternative proves (today, the single alternative's proof). Later or-patterns become a parser addition, plus one diagnostic for arms that no order can satisfy.
    - Combining field relations: any disjoint field means the arms are disjoint; otherwise any partial leaf is an error; otherwise collect the non-equal directions: none = identical, only sub = sub, only super = super, both = same weight. A nested mixed result propagates upward as same weight.
  - **Analyzer, match checking (`analysis/patterns.rs`):**
    - `analyze_match`: dispatch a struct (or pointer-to-struct via `matched_through_pointer`) scrutinee to the new path. Today it reaches `UnsupportedMatchScrutinee`, whose note "`match` supports enums, integers, and `bool`" must be updated.
    - `analyze_enum_match`, `analyze_anonymous_enum_match`, `analyze_value_match`: replace per-kind overlap/coverage bookkeeping with one flow:
      1. resolve each arm into a pattern-space node;
      2. apply rules 1, 2, 3 and 5 pairwise in source order;
      3. walk arms in order subtracting from the remaining space; an empty intersection is rule 4;
      4. the final remainder drives the non-exhaustive error, `else` reachability and `..` validity.
    - Remove the "`..` resolved separately, anywhere" special case and `CatchAllRangeNotInferable`.
    - `resolve_variant_pattern`, `analyze_value_pattern`, `const_eval_pattern`, `tag_variant_condition`, `value_cmp_condition` are reused for leaves.
  - **Analyzer, conditions emitted per arm (`CheckedMatchArm.conditions`):**
    - An ordinary arm emits its own positive pattern as one AND group, ordered outer to inner. The outer tag test precedes any projection into that variant's fields, so the short-circuit order guards each projection.
    - Positions that are "any" emit no test.
    - **Exception, to preserve the invalid-tag invariant:** a top-level `..` arm and an enum `else` emit one group per scrutinee variant/member that still has uncovered cells. This is exactly today's shape for flat matches. For value/struct scrutinees, `..` emits no condition.
  - **Analyzer, `CheckedMatchRemainder`:** `IllegalEnumTag` when the match's tests include any enum/anonymous-enum tag test at any depth, otherwise `Covered`.
  - **Analyzer, field-path refinement:**
    - Extend the narrowed binding (`context.rs` `VarBinding`, `analysis/mod.rs` `declare_narrowed_binding`, called from `analyze_enum_match`/`analyze_anonymous_enum_match`) with refined field paths: a field-name path mapped to a refined type (`ResolvedType::Enum { variant: Some(i) }` or an anonymous-enum member).
    - `analysis/places/mod.rs` `analyze_place` / `places/fields.rs` `resolve_field_projection` use the refined type when a projection chain from a narrowed root matches a refined path. This also applies through a pointer scrutinee's auto-deref.
    - Refinement stays in the analyzer context. `ResolvedType` is not changed.
  - **Analyzer, widening and assignment:**
    - `analysis/exprs/mod.rs` (`&mut` handling around `narrowable_place`/`widen_variable`): `&mut` to a refined path or any prefix of it drops the refinements at and below that path. `&mut binding` already widens everything.
    - A refined field path, or any prefix down to the root, is not an assignment target while the proof holds. That extends the existing narrowed-binding rule. Assigning *inside* a refined field (`res.value.value = 3`) stays legal.
  - **Analyzer, errors (`error/kind.rs`, `error/render.rs`):**
    - Reuse `OverlappingMatchArm` for rule 1, adding the conflicting field path to the label.
    - Add kinds for rule 2 (more specific arm after the arm containing it, with help "move this arm above ..."), rule 4 (unreachable arm, pointing at the covering arms), rule 5 (duplicate arm), a non-exhaustive nested match listing missing cells as pattern text, field-pattern errors (unknown/invisible field, non-patternable field type, header/tag field named in a pattern, a field missing with no `..`, path type that doesn't match the position), and refined-path assignment.
    - Keep `CatchAllPatternRedundant` for an unreachable `..` and `MultipleCatchAllPatterns`.
    - **Diagnostics for flat matches must stay byte-identical** to current output, except where the rule itself changed.
- **Interfaces/invariants:**
  - **Patternable positions:** enum, anonymous enum, struct, and integral/bool/char. Only runtime fields may be named in a pattern: shared dynamic fields plus variant body fields for enums, visible fields for structs. This mirrors construction, which never supplies header/tag values. Union fields and other types are not patternable.
  - **Anonymous-enum positions:** a subpattern may be a member type, a struct pattern whose struct is a member, or a variant pattern whose enum is a member. Each selects that member.
  - **Invalid tags:** a nested enum tag is tested only where a subpattern names a variant/member. An invalid nested tag fails those tests. If no later arm accepts the value, the `IllegalEnumTag` remainder panics with `invalid enum tag in match`. An arm that leaves the field as "any" does not test it. The scrutinee's own tag is always tested, including by `..` and `else`, as today.
  - **Generic bodies:** follow however `patterns.rs` already handles generic enum scrutinees. If checking a field typed by an unresolved type parameter requires new template machinery, stop and escalate.
  - `CheckedMatch`/`CheckedMatchArm` shapes stay unchanged, and MIR/codegen are untouched.
- **Out of scope:**
  - matching several scrutinees at once and or-patterns (`|`); the data model only keeps room for them;
  - named-constant/`comp` patterns: `grammar.md` mentions constant-value patterns, but `const_eval_pattern` accepts literals only. That mismatch is a separate task;
  - bindings or `@`-style captures in patterns;
  - patterns in `if`/`while`/declarations;
  - guards;
  - float patterns;
  - changing the `?` operator or `for ... in` (they build `CheckedMatchArm`s directly in `analysis/stmts.rs`; leave them alone).
- **Risks/open questions (escalate, don't decide):**
  - The macro expander or macro `pattern` fragments may need a grammar decision beyond a recursive expansion.
  - Cell explosion in subtraction across wide integral fields × many fields. Intervals keep it polynomial per field, but if real cases become slow, report it rather than adding heuristics.

## Implementation Plan

1. **Pattern-space engine.** Generalize `compiler/omega-analyzer/src/exhaustiveness.rs` into the engine described above. Give it crate-local unit tests for relations, the combination rule, subtraction and cell rendering. No callers change yet; the existing interval API keeps working.
2. **Parser + HIR.** Add the `Fields` pattern form and its parsing, including the type-probe terminator change, macro expansion and HIR lowering. Add parser tests for: nested fields, `..` rest, `;` termination, an anonymous member type at a nested position, and `if`/`match` condition positions where struct literals are restricted. Until step 3, the analyzer may reject `Fields` with `PatternNotEnumVariant` so the tree keeps building.
3. **Unified match analysis.** Route enum, anonymous-enum and value matches through the engine. Implement rules 1–5, `..` as most general, the condition emission (including the `..`/`else` exception) and the remainder choice. Run the existing `t07`, `t27*`, `t41*` cases. They must pass unchanged, except for expectations that encode the removed contiguous-range rule (none are known; check).
4. **Struct scrutinees.** Add the `analyze_match` dispatch and the `UnsupportedMatchScrutinee` note update.
5. **Field-path refinement.** Implement refined paths on narrowed bindings, the projection lookup, `&mut` prefix widening and the refined-path assignment rejection.
6. **Diagnostics.** Add the new kinds and their rendering.
7. **Docs:**
   - **`enums-and-pattern-matching.md`:**
     - Enum patterns: the field-pattern form and field-path refinement.
     - Exhaustiveness and overlap: rules 1–5 replace "Arms must partition... There is no first-match-wins rule"; state the reading-order guarantee.
     - Bare `..`: most general arm, must be last, contiguity rule and its example removed; the anonymous-enum coverage text adjusted to match.
     - Invalid tags: nested tests.
     - Anonymous-enum matching: nested members.
     - Struct scrutinees in `## match`.
   - **`grammar.md` `### match`:** pattern productions for field patterns.
   - **`bindings-and-mutability.md` `## Mutable places`:** refined field paths, prefix widening, assignment rule.
   - **`control-flow-and-operators.md` `## match`:** add struct scrutinees if it lists supported kinds.
   - **`docs/architecture/semantic-analysis.md` §Pattern matching and exhaustiveness (and the `CheckedMatchRemainder` paragraph):** the pattern-space engine, condition emission, remainder rule.
   - **`docs/guide/quick-reference.md` `## Enums` / `## if, match, and loops`:** one nested-pattern example.

## Testing

- **New/changed cases (root `tests/`, language conformance):**
  - `tests/t53_nested_patterns/` (positive, `expected.stdout`):
    - nested enum-in-enum arms that are exhaustive with no catch-all;
    - integral, bool and char field subpatterns;
    - a struct scrutinee and nested struct patterns;
    - a more specific arm before the general one, with each running for its values;
    - same-weight arms where the earlier arm wins on the overlap (the origin case);
    - `..` last;
    - `0 => ..., .. => ...` on an integer;
    - `5 => ..., 0..<10 => ...`;
    - reading `res.value.value` through field-path refinement;
    - matching through a pointer scrutinee;
    - an anonymous enum member at a nested position;
    - writing inside a refined field.
  - `tests/t53b_nested_pattern_errors/` (`expected.stderr`), one error per construct:
    - leaf partial overlap;
    - more specific arm after general;
    - duplicate arm;
    - an arm covered by the union of earlier arms (`a = true; ..` / `a = false; ..` / `Pair { .. }`, and the three-range case);
    - `..` not last;
    - a non-exhaustive nested match whose missing cells are rendered as patterns;
    - unknown field, header field and union field named in a pattern;
    - a field missing without `..`;
    - assignment to a refined path;
    - after `&mut res.value`, the refined access `res.value.value` rejected.
  - `tests/t53c_nested_pattern_invalid_tag/` (`expected.stdout`/`expected.stderr`/`expected.status` plus a C helper, mirroring `tests/t41c_invalid_enum_tag_catch_all/`): an invalid tag in a nested field with no accepting arm panics with `invalid enum tag in match`. An arm leaving that field as "any" still runs.
- **Specification trace:** each case maps to the rewritten `### Exhaustiveness and overlap`, `### Bare .. catch-all`, `### Enum patterns`, `### Invalid tags` (enums-and-pattern-matching.md) and `## Mutable places` (bindings-and-mutability.md).
- **Negative/diagnostic cases:** every negative case checks the full rendered diagnostic. A bare compile failure doesn't count.
- **Regression coverage:**
  - existing `t07_enums_and_pattern_matching`, `t27_anonymous_enums`, `t27b_anonymous_enum_errors`, `t41*`, and any `for ... in` / `?` cases;
  - `compiler/omega-driver/tests/runtime_checks.rs`, which references the match error kinds;
  - analyzer/parser crate tests.
- **Commands:** `cargo test -p omega-parser -p omega-hir -p omega-analyzer -p omega-driver`, then `./bin/test-runner t07_enums_and_pattern_matching t27_anonymous_enums t27b_anonymous_enum_errors t41c_invalid_enum_tag_catch_all t53_nested_patterns t53b_nested_pattern_errors t53c_nested_pattern_invalid_tag`, then `just test-all`.
