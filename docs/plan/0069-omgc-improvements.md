# omgc CLI polish and `<name>=<dir>` identity separator

## Task Description
- **Deliverable:**
  1. Every `omgc` failure, including command-line errors, is printed through the same `omega_diagnostics::Renderer` as compile diagnostics. That gives it the same colored `error:` header and `= help:` / `= note:` footers, and it can report several errors at once.
  2. The declared-identity separator for the entry argument and `--import` changes from `:` to `=`. The forms become `omgc [<name>=]<entry-dir>` and `--import=[<name>=]<dir>`.
- **Purpose:**
  - Today, CLI errors come from a bare `eprintln!("error: {message}")` in `compiler/omgc/src/main.rs`. They have no color and put long explanations after `--` on a single line. Multi-error output from `-D` is joined with `\n`, so only the first line gets an `error:` prefix. The output of `omgc pkg` (no `-o`) is `error: the -o <dir> flag is required`, which looks nothing like the rest of the compiler's output.
  - `:` is part of Windows paths (`C:\...`). The current parser works around this with a drive-letter special case (`is_windows_absolute_path`). `=` cannot appear in a Windows drive prefix, and `-D<name>=<literal>` already uses it as omgc's name/value separator.
- **Chosen direction:**
  - `cli.rs` and `app.rs` build `omega_diagnostics::Diagnostic` values instead of `String`s. They split each message into a short headline plus `help`/`note` footers.
  - `main.rs` renders the diagnostics using the same color detection as `app::compile`.
  - `split_declared_root` splits at the **first** `=`, and the drive-letter special case is deleted.
- **Rejected alternatives:**
  - *Keep accepting `:` as a legacy alias.* This would make two spellings for one mechanism and would keep the Windows ambiguity alive. Plan 0025 dropped `--extern` without an alias, and this change follows the same approach. `core:deps/core` becomes an ordinary path.
  - *A CLI-parsing crate such as clap.* This adds a new dependency and changes flag syntax (for example, `-D` handling) for a cosmetic goal. The existing renderer already provides the target look.
  - *A dedicated error for old `name:dir` spellings.* This is a special case with no lasting value. The existing "package root contains no modules" error already names the bad path.

## Technical Details
- **Initial context boundary:** `compiler/omgc/src/{main.rs,app.rs,cli.rs}`, `compiler/omgc/tests/`, and the public API of `compiler/omega-diagnostics` (`Diagnostic::error`, `with_help`, `with_note`, `Renderer::render`, and the public `message` field). No other crates, except for the two message strings listed below.
- **Affected files/symbols:**
  - `compiler/omgc/src/main.rs`:
    - Render `AppError` diagnostics with `Renderer::new(colors)` and an empty `SourceRegistry`. Print a blank line between diagnostics, as `render_compile_errors` does.
    - Share the color predicate (`stderr().is_terminal() && NO_COLOR unset`) with `app.rs` through one helper instead of duplicating it.
  - `compiler/omgc/src/app.rs`:
    - `AppError::Message(String)` becomes `AppError::Diagnostics(Vec<Diagnostic>)`. Keep `From<String>` so that `omega_codegen::generate(request)?` still works; it maps to a single `Diagnostic::error`.
    - Rewrite the two entry-basename messages (lines ~52–64) and the `write_artifacts` "not a directory" / "failed to write" messages as a headline plus footers.
    - Final success line (`println!("Saved {} artifact(s) to: …")`):
      - Print it with the same right-aligned green status format as `verbose_step` (for example, `       Saved 3 artifact(s) to out/`) on stderr, so all status output shares one stream and one style.
      - In verbose mode, the `Finished` step and the saved line should not repeat each other; keep one of them.
  - `compiler/omgc/src/cli.rs`:
    - `parse` and `parse_compile` return `Result<_, Diagnostic>`. `resolve_definitions` returns `Result<_, Vec<Diagnostic>>` with one diagnostic per bad option. `validate_module_name`, `parse_entry`, and `parse_import` also move to `Diagnostic`.
    - Put the usage line in one `const USAGE` that both `print_help` and the missing-argument errors use.
    - Suggested messages (the developer may refine the wording, but keep the structure):
      - No entry directory: `error: no package directory given`, with `help: usage: omgc [<name>=]<entry-dir> -o <output-dir> [OPTIONS]` and `note: run 'omgc --help' for all options`.
      - No `-o`: `error: no output directory given`, with `help: pass '-o <output-dir>'; omgc writes one artifact per source file into it`.
      - Unknown flag: keep the headline, and add `help: run 'omgc --help' to list the accepted options`.
      - Invalid module name / definition name: the headline names the offending spelling and its origin. The identifier rule goes in a `note:`, and the override hint (`pass <name>=<dir>` / `--import=<name>=<dir>`) goes in a `help:`.
    - `split_declared_root`:
      - Split at the first `=`. An empty name is an error: `the name before '=' cannot be empty`.
      - Delete `is_windows_absolute_path` and its comment.
    - Update the doc comment on `parse_entry` and the `help_option` strings (`[<name>=]<entry-dir>`, `--import=[<name>=]<dir>`).
  - `compiler/omega-analyzer/src/resolver.rs:217` and `compiler/omega-analyzer/src/error/render.rs:1083`: `--import={}:<path>` becomes `--import={}=<path>`. These are text-only edits; no analyzer logic changes.
  - Build/tooling invocations:
    - `justfile` lines 35–37, 74, 76, 98.
    - `bin/test-runner:47`.
    - `bin/check-platform:90,452`.
    - Rust integration tests: `compiler/omgc/tests/aligned_allocation_glue.rs:197-198`, `generic_overload_linkage.rs:206`, `shared_library_export.rs:159`, and the doc comment at `output_layout.rs:150`.
  - Docs that spell the form:
    - `docs/guide/compiler-cli.md` (usage block, identity section, examples).
    - `docs/guide/platform-glue.md`.
    - `docs/language/modules-and-imports.md` (lines 7, 11, 13, 200).
    - `docs/architecture/runtime-and-platform.md:54,171-177`.
    - `docs/architecture/symbol-mangling.md:98`.
    - `docs/architecture/module-driver-and-linkage.md:559`.
  - Comments that spell the form: `compiler/omega-driver/src/roots.rs:23` and `runtime/plat/libc/libc.omg:8`.
  - **Do not edit `docs/plan/`.**
  - `tests/t12d_explicit_name_collisions/expected.stderr`: both `--import=nosuchpackage:<path>` occurrences change to `=`.
- **Interfaces/invariants:**
  - Only the first `=` separates name from path. This matches the `-D` rule, so `name=./a=b` means name `name` with dir `./a=b`.
  - A bare path that contains `=` needs an explicit name. Document this in `compiler-cli.md`.
  - The declared name is still validated by `validate_module_name`. Name validation, basename inference, and output layout do not change.
  - Exit codes do not change: 0 on success, 1 on any failure.
  - `--help` still goes to stdout. The help text is still only colored on a TTY with `NO_COLOR` unset.
  - Every CLI error goes to stderr, with color under the same conditions.
  - When color is off, the output has no escape codes, so `expected.stderr` comparisons stay stable.
- **Out of scope:**
  - Changing the renderer's layout in `omega-diagnostics`.
  - Rewording analyzer or driver diagnostics beyond the two `--import` hints.
  - New flags, and changes to `-D` syntax.
  - Using a CLI library.
- **Risks/open questions:**
  - If any consumer outside this repo parses stdout for `Saved …` (for example, the separate shared-library consumer project), moving the line to stderr breaks it. If the developer finds such a dependency inside this repo, keep the line on stdout and only restyle it. Do not add a flag for this.

## Implementation Plan
1. **Separator change.**
   - Update `split_declared_root`, delete `is_windows_absolute_path`, and update the help/usage strings and every `name:dir` spelling in `cli.rs` and `app.rs`.
   - Update the `cli.rs` unit tests. `windows_drive_letter_is_not_parsed_as_a_module_name` becomes a test that `C:\omega\core` has no name and `core=C:\omega\core` splits correctly. Also add tests for an empty name (`=src`) and first-`=` splitting.
   - In the same step, update `justfile`, `bin/test-runner`, `bin/check-platform`, the omgc integration tests, the two analyzer hint strings, and `t12d` `expected.stderr`, so that the tree still builds and passes tests.
2. **Diagnostic-based errors.**
   - Convert `cli.rs` and `app.rs` error paths to `Diagnostic` as described above, and add `const USAGE`.
   - Make `resolve_definitions` return one diagnostic per failing option.
   - Change `main.rs` to render `AppError::Diagnostics` with the shared color helper.
   - Update `cli.rs` unit tests that match on error text so they match on `diagnostic.message` or on footers.
3. **Status line.**
   - Restyle the success line as described in Technical Details, and remove the duplicate verbose `Finished` line.
4. **Docs.**
   - Update the listed guide, language, and architecture docs, and the two source comments, to `<name>=<dir>`.
   - In `docs/guide/compiler-cli.md`:
     - State the first-`=` rule and that a path containing `=` needs an explicit name.
     - Update the example `omgc` error output if the guide shows any.
   - In `docs/language/modules-and-imports.md`, change only the tool-spelling examples. The semantic rules do not change.

## Testing
- **New/changed cases:**
  - Unit tests in `cli.rs`:
    - `=` splitting, first-`=` precedence, empty name rejection.
    - A Windows drive path with and without an explicit name.
    - A `name:dir` argument is now treated as a bare path, so no name is inferred from it.
    - Each missing-argument error has the expected headline and a help footer that contains `USAGE` / `-o <output-dir>`.
    - Several bad `-D` options produce several diagnostics.
  - `compiler/omgc/tests/output_layout.rs::the_help_and_usage_text_describe_an_output_directory`:
    - Assert the new missing-`-o` headline and the `-o <output-dir>` help.
    - Assert that stderr contains no `\x1b` when not on a TTY. The test harness pipes stderr.
- **Specification trace:** no language semantics change. `modules-and-imports.md` only updates its tooling spelling. The `t12d` `expected.stderr` update covers the analyzer hint text.
- **Negative/diagnostic cases:**
  - Run `omgc pkg`, `omgc` with no arguments, `omgc --bogus`, and `omgc foo-bar=src -o out`.
  - Check the full stderr text, not only the failure status.
- **Regression coverage:**
  - `cargo test -p omgc` (this includes `compiler_definitions.rs`, which checks the `more than once` / `16-bit` substrings; those must still appear).
  - `cargo test -p omega-analyzer` in case any test asserts on the hint.
  - `./bin/test-runner t12d_explicit_name_collisions`.
- **Commands:**
  - Run `just test-all` last. It rebuilds the runtime through the updated `justfile` `--import=core=…` lines, which exercises the new separator end to end.
  - Optionally run `bin/check-platform` if its imports changed.
