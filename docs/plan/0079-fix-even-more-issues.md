# Generic-argument lists on paths and calls

## Task Description
- **Deliverable:** fix the live generic-argument and path-resolution gaps listed below, lift two normative restrictions that exist only because of implementation limits, and remove entries from `docs/issues/` that triage showed already fixed.
- **Purpose:** each item makes a natural, well-defined call shape fail, usually with a misleading diagnostic:
  - `println$(mid<u16, u8>())` fails to parse;
  - `Owner<i32>::name<u8>(..)` is reported as "comparison operators are non-associative";
  - `m::Box::make(5i64)` says "expects 1 generic argument(s), found 0" although `Box::make(5i64)` works;
  - `mid<_, u8>()` says "`T` is not determined" despite `T = u16`;
  - `Make::make(3)` on `spec Make<T>` leaks "cannot find type 'T'";
  - `f: (i32) => *str = T::pick` selects a conformance method over an inherent generic `pick<U>`.
- **Chosen direction (one owner per fact):**
  1. **Macro arguments are delimited by the fragment grammar**, exactly where an ordinary call's arguments would be delimited, not by a token-level comma split. The invocation keeps its raw token run; expansion splits it by parsing each parameter's fragment kind (`expr`, `type`, `ident`, `path`) in order and then requiring `,` or the end. `<`/`>` ambiguity is then decided by the same expression/type parser that decides it everywhere else.
  2. **A path may carry one generic argument list on its owner segment and one on its function segment** (`Owner<A>::name<B>(...)`, `m::Owner<A>::self::name<B>(...)`). The single `generic_args`/`args_at` pair in `ExprPath` becomes a segment-indexed list. Instantiation identity already includes both lists (`generics.md`), so the one-list rule was purely syntactic.
  3. **A generic owner's arguments are inferred from the call whatever spelling names the owner.** This covers module-qualified paths, imports, aliases and anchors, so `m::Box::make(5i64)` behaves like `Box::make(5i64)`. The owner prefix is resolved with the same absolute-item-path resolution other qualified calls use (`Context::resolve_absolute_item_path`, as `calls/spec.rs::resolve_spec_qualified_call` does), not only through `path.head`.
  4. **An unbound defaulted generic parameter may precede a bound one.** Inference results become positional with gaps (`Vec<Option<ResolvedGenericArg>>`). Every gap is filled from its declared default, in declaration order, evaluated in the **declaring** module with the preceding arguments substituted. There is one driver helper for this, unifying `items/resolution.rs::pad_generic_defaults` (trailing-only today) and `complete_overload_arguments` (already gap-aware).
  5. **Omitted or `_` spec arguments in a qualified spec call are determined by the target's conformances.** For `P::f(...)`, `<S : P>::f(...)`, `P<_, u8>::f(...)` and `<S : P<_>>::f(...)`:
     - Determine the target exactly as today (receiver or expected type), then take as candidates every conformance of the target to `P` whose arguments agree with the written ones.
     - Select among their `f` declarations with the existing overload resolution.
     - No candidate is "does not conform", more than one surviving overload resolution is ambiguous, and the existing diagnostics are reused.
     - This mirrors the existing `<S : _>` rule (candidate = spec + its arguments) instead of adding a second inference mechanism.
  6. **Inherent declarations take precedence over conformance declarations in value selection**, generic ones included. Conformance providers are consulted only when the owner has neither concrete inherent candidates nor inherent generic templates under that name in the selected namespace.
- **Rejected alternatives (matter for review):**
  - Teaching the macro token splitter to balance `<`/`>`. Impossible in general (`m$(a < b, c > d)`) and a second, divergent disambiguation rule.
  - Joint owner-and-method inference (`Owner::name(…)` where `name` is itself generic and the owner's arguments are omitted). It is **deliberately deferred**: the driver's `generic_owner_function_signature` rewrites `Self` as `Owner<T…>` keyed by parameter *names* (`resolver.rs`, `rewrite_self`), so a member parameter that shadows an owner parameter would silently capture it. The approved `GenericParamId` refactor is its prerequisite. In this batch, an owner-inference call on a generic member stays declined exactly as today.
  - Adding a fresh analyzer→resolver query per default. The driver already owns default evaluation in the declaring module; extend that, do not duplicate it.
  - Inferring spec arguments from call-argument unification against the requirement signature. Conformance enumeration is already the semantic source of truth (`<S : _>`), and it is the only rule that cannot select a non-existent conformance.

## Technical Details
- **Initial context boundary:**
  - Item 1: `compiler/omega-parser` only.
  - Item 2: `omega-parser` (`ExprPath`) plus the analyzer consumers of `args_at`.
  - Items 3–6: `compiler/omega-analyzer/src/analysis/{calls,paths.rs}`, plus `omega-driver/src/items/resolution.rs` and `omega-driver/src/resolver.rs` for item 4 only.
  - Language docs: `docs/language/generics.md` (§ inference holes, about lines 150–195; generic members, about lines 340–356), `docs/language/specs-and-conformance.md` § Calling conforming functions (about lines 122–165), `docs/language/macros.md` § Parameters and repetition, `docs/language/functions.md` § Selecting a function value.
  - Read `docs/guide/quick-reference.md` before writing `.omg`.
- **Affected files/symbols (verified):**
  1. **Macro arguments:**
     - `omega-parser/src/parser/macro_syntax.rs::parse_macro_invocation` / `parse_macro_args` / `capture_token_run` split on depth-0 commas. Keep balanced-bracket capture of the whole `(...)` run, but stop splitting.
     - `MacroInvocationExpr.args` (`ast/expression.rs`) becomes the single raw run.
     - `omega-parser/src/macros/expander.rs::substitute_invocation` splits it using the signature: each fixed parameter parses its fragment with the same entry points `macros.rs::validate_fragment` uses, then `,` or end; a variadic parameter repeats.
     - Arity errors (`ArgCountMismatch`) and `FragmentMismatch` keep their wording. A trailing `,` and an empty argument stay errors as today.
     - Fold `validate_fragment` into the splitter rather than parsing twice.
     - Audit every other reader of `MacroInvocationExpr.args`, in particular builtin macros (`def.builtin` in `substitute_invocation`) and any parser/HIR code that inspects invocation arguments, and give them the same split.
  2. **Two generic lists on a path:**
     - `omega-parser/src/ast/identifier.rs::ExprPath { generic_args, args_at }` becomes a segment-indexed list, strictly increasing by segment and at most one list per segment. Add small accessors (`args_on(segment)`, `is_empty()`).
     - `parser/expression.rs::parse_expr_path` (about line 728) only tries `try_parse_generic_args` while `generic_args.is_empty()`; allow a list after every segment.
     - Update the analyzer consumers of `args_at`: `analysis/asm.rs`, `calls/generic.rs` (×4, notably `resolve_generic_method_call`'s `owner_generics_written`/`function_generics_written` and `resolve_generic_owner_function_call`), `calls/overload.rs` (×3), `calls/spec.rs` (×1), `literals.rs` (×3), `paths.rs` (×5).
     - Owner list plus function list is accepted wherever a type-qualified function path is. A list on any other segment (a module segment, `self`, a variant) gets one dedicated analyzer error naming where lists are allowed; it must never fall through to a comparison parse error.
     - Within this batch the owner list must be fully written when a function list is also written, because joint inference is deferred (see Rejected alternatives). An owner list with `_` holes together with a generic member is reported with the existing "not determined"-style diagnostic, not silently truncated.
  3. **Module-qualified owner inference:** `calls/generic.rs::resolve_generic_owner_function_call` accepts only `[member]` / `[self, member]` tails and resolves the owner from `path.head` through `anchored_prefix`/import aliases. Resolve the full owner prefix (everything before the member segments, with its own written list if any) through `resolve_absolute_item_path`, and keep the ambient-`core` fallback and visibility-bypass logic. `generic_owner_function_signature` already takes an absolute path. Keep the `function_generics.is_empty()` decline.
  4. **Interior defaults:**
     - `omega-analyzer/src/generics.rs::resolve_inferred_generic_args` returns positional `Vec<Option<ResolvedGenericArg>>`: `None` for an unbound *defaulted* parameter at any position, and still an error naming an unbound parameter without a default. Update its doc comment.
     - Function paths already accept gaps (`prepare_generic_call`, `instantiate_overload` → `complete_overload_arguments`).
     - For aggregate/owner instantiation that goes through `ModuleResolver::resolve_item(.., generic_args: &[ResolvedGenericArg], ..)`, make the driver's `pad_generic_defaults` accept a positional gap list and fill every gap. Its callers are `resolver.rs` about line 585, `items/methods.rs` about line 148, and `items/resolution.rs` about line 103.
     - Choose the narrowest interface change. Either `resolve_item` takes `&[Option<ResolvedGenericArg>]` (prefix-only callers wrap in `Some`), or a dedicated `complete_generic_args(absolute, gaps)` query is used before `resolve_item`. Prefer whichever touches fewer call sites, and record the choice in `docs/architecture/semantic-analysis.md` only if the resolver contract text there mentions prefix padding.
     - `complete_overload_arguments` and `pad_generic_defaults` must share one gap-filling routine.
  5. **Spec arguments in qualified spec calls (`calls/spec.rs`):**
     - `resolve_spec_qualified_call` currently resolves `spec_args` only when written, then calls `flatten_spec` with whatever it has, which is why an omitted list leaks the spec's own `T`.
     - Determine the requirement's *shape* (name, receiver mode, returns-`Self`) from the spec declaration without substituting arguments.
     - Determine the target exactly as `resolve_static_spec_call` / `resolve_instance_spec_call` already do.
     - Enumerate the target's conformances to that spec, using the same resolver capability `infer_qualified_spec` (about line 391) uses for `<S : _>`, and keep those whose arguments match the written ones. A `_` matches anything, and a written comp value is compared canonically.
     - Pool their methods named `f` (in the receiver namespace already selected) and run the existing selection (`resolve_overload` when more than one).
     - `resolve_fully_qualified_spec_call` gets the same treatment for `<S : P>` / `<S : P<_>>`. `<_ : P<_>>` stays rejected, like `<_ : _>`, because nothing infers both halves; `<*_ : P>` stays rejected.
     - Remove the "nothing is inferred here" rejection for spec-application holes at its source (find where `InferenceHoleNotAllowed`-style errors are raised for spec applications).
  6. **Inherent precedence:** in `analysis/paths.rs::resolve_type_member` (about line 943), query `generic_method_candidates` *before* the conformance fallback and enter that fallback only when both `candidates` and the inherent templates are empty. Confirm whether `generic_method_candidates` can return conformance-provided generic methods (meet blocks now accept explicit generics); if it can, split inherent from conformance templates so precedence holds for templates too. Check that the call path (`Type::pick(1)`) already applies the same precedence; if not, fix it with the same rule.
- **Normative docs to change:**
  - `generics.md` about line 352: replace "One path carries at most one written argument list, so an owner and its function cannot both be written explicitly in one call" with the two-list rule. Note that the owner list must be complete when the member is generic and its owner list is written; joint inference is not yet supported, and that belongs in `docs/issues/`, not the chapter.
  - `generics.md` § holes: move "a spec application's arguments (`Spec<_>::f`)" from the rejected list to the accepted list, with a link.
  - `specs-and-conformance.md` § Calling conforming functions: state the conformance-enumeration rule for omitted or `_` spec arguments; replace "A hole inside either half (`<S : P<_>>`, `<*_ : P>`) is not inferred" accordingly (`<*_ : P>` stays not inferred).
  - `macros.md`: one sentence saying arguments are delimited by each parameter's fragment grammar, so commas inside generic argument lists belong to the argument.
- **`docs/issues/` maintenance:**
  - Remove the entries fixed here: inherent precedence; `Owner<A>::name<B>`; defaulted `_`; holes in spec application.
  - Remove entries triage found already fixed: "deeper `module::Type::function(...)` resolves without overload disambiguation" (verified working) and "Qualified diagnostics still shorten spec-object and anonymous-enum members" (verified: candidates print `<S : g12::P<*spec g12::a::X>>::f`).
  - Rewrite "A generic declaration in a `meet` or `primitive` block is still rejected": explicit generics in `meet` blocks now work. The remaining failure is `spec S` parameter sugar on a conformance method satisfying a generic requirement, which leaks `cannot find type '$Param1'`. Link it to the `$ParamN` design-debt entry.
  - Rewrite "A generic method call cannot infer its owner's generic arguments" to the deferred joint-inference case, noting the shadowing prerequisite.
- **Interfaces/invariants:**
  - No ABI, mangling, MIR or codegen change.
  - Existing accepted programs keep their meaning; this batch only accepts more and improves diagnostics.
  - Defaults are evaluated in the declaring module, never the use site.
  - Instantiation identity is unchanged: `mid<_, u8>()` is the same instantiation as `mid<u16, u8>()`.
  - Determinism: candidate pooling in item 5 iterates conformances in the resolver's existing deterministic order.
- **Out of scope:**
  - Joint owner-and-method inference.
  - `spec S` sugar in conformance methods.
  - `reveal` in type position (belongs to the reveal refactor).
  - Generic `foreign` functions.
  - The `ZeroSizedAggregate` label.
  - Overloaded statics under owner inference (`generic_owner_function_signature` declines more than one candidate).
- **Risks/open questions (stop and escalate):**
  - Item 1: if any macro in `runtime/` relies on the current split, i.e. it passes an argument that parses as a longer fragment and was previously cut at a comma, stop and report the case.
  - Item 4: if completing an interior default requires changing more than the resolver methods named above, stop and report the needed interface.
  - Item 5: if a target's conformance list cannot be enumerated for one spec without proving unrelated conformances (cost or cycles), stop and report.

## Implementation Plan
1. **Macro arguments by fragment grammar.** Parser AST change, expander split, builtin audit, parser tests. Rebuild the runtime and run the macro conformance cases (`t18*`, `t45t*`, `t60*`).
2. **`ExprPath` segment-indexed generic lists.** Parser change, mechanical update of analyzer consumers with no behavior change, then the analyzer rule: owner+function lists accepted, misplaced lists diagnosed.
3. **Module-qualified owner inference.**
4. **Positional generic arguments with gaps.** Analyzer return type first, then driver gap filling and the resolver interface.
5. **Spec-argument determination in qualified spec calls.**
6. **Inherent precedence in value selection.**
7. **Normative docs and the `docs/issues/` maintenance above.**

## Testing
- **Root conformance (new, next free numbers after `t61`):**
  - `t62_macro_generic_arguments`: `println$(mid<u16, u8>())`, a `type` fragment `Map<K, V>`, a comparison argument `m$(a < b, c > d)` that still yields two arguments, and a nested invocation inside a generic argument. Output: `expected.stdout`.
  - `t63_owner_and_function_arguments`: `Owner<i32>::name<u8>(1, 2)`, `m::Owner<i32>::self::name<u8>(&o, 2)`, prints both.
  - `t63b_misplaced_generic_arguments`: a list on a module segment and on `self`; full `expected.stderr`.
  - `t64_qualified_owner_inference`: `m::Box::make(5i64)`, an import-aliased owner, an anchored `root::m::Box::make`, each printing its value.
  - `t65_interior_generic_defaults`: `mid<_, u8>()` prints `3`; an aggregate `Pair<_, u8>` with a defaulted first parameter; a default that refers to an earlier parameter (`<T, U = T>` style) used through a gap.
  - `t66_spec_argument_inference`:
    - `x : W = Make::make(3)`, `<W : Make>::make(3)`, `<W : Make<_>>::make(3)`;
    - an instance requirement (`Spec::f(&v)`);
    - a type conforming to both `Make<i32>` and `Make<u8>`, where overload resolution selects by argument type.
    - Prints which conformance ran.
  - `t66b_spec_argument_inference_errors`: no matching conformance, an ambiguous selection between two conformances, `<_ : Make<_>>`; full `expected.stderr`.
  - `t67_inherent_value_precedence`: the `pick<U>` versus `meet Picker` value case, and the matching call case; both print `inherent`.
- **Specification trace:**
  - t62: `macros.md`.
  - t63/t63b: `generics.md` generic members (new two-list rule).
  - t64: `generics.md` generic members ("every spelling that names a type selects an owner").
  - t65: `generics.md` defaults and holes ("a declared default applies to it").
  - t66: new `specs-and-conformance.md` rule.
  - t67: `functions.md` § Selecting a function value plus `specs-and-conformance.md` (inherent before conforming).
- **Crate tests:**
  - `omega-parser`: invocation splitting, `ExprPath` multi-list parsing, and that `a < b, c > d` stays two comparisons.
  - `omega-analyzer`: unit tests for `resolve_inferred_generic_args` gaps.
  - `omega-driver/tests/`: gap filling evaluated in the declaring module (default naming a module-private type, used from another module).
- **Regression coverage:**
  - `t47*` inference-hole cases (their `expected.stderr` may legitimately change only where this batch now accepts a program; justify each change).
  - `t18*` macros; `t12*` imports; `t26*` aliases; spec/conformance cases (`grep -l meet tests/*/*.omg`); `t61_same_named_specs`.
  - `cargo test -p omega-parser -p omega-analyzer -p omega-driver`.
- **Commands:**
  - `cargo build`, then `just build-runtime` after step 1.
  - Focused `./bin/test-runner <cases>` per step.
  - `just test-all`, `cargo fmt --check`, `git diff --check`.
