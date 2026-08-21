# Inline assembly

`asm` embeds target assembly text directly into a function body. It is a statement, not an expression: it produces no Omega value and cannot be used where a value is expected.

```omega
asm(
    reg(<expr>),
    reg(<expr>, "<physical register>"),
    const(<comp binding>),
    clobber("<physical register>"),
    ...
) => {
    <target assembly using $operand bindings>
}
```

## Syntax

```ebnf
asm-statement = "asm", "(", [ asm-descriptor, { ",", asm-descriptor }, [ "," ] ], ")",
                "=>", "{", asm-body, "}" ;

asm-descriptor = "reg", "(", expression, [ ",", string-literal ], ")"
                | "const", "(", identifier, ")"
                | "clobber", "(", string-literal, ")" ;
```

`asm`, `reg`, `const`, and `clobber` are contextual: they are ordinary identifiers everywhere else and only acquire meaning as the head of an asm statement or descriptor. See [`lexical-structure.md`](lexical-structure.md#contextual-keywords).

`asm-body` is **not Omega syntax**. Once the outer `asm(...) => {` shape is recognized, everything up to the matching outer `}` is captured as raw text for the target assembler, using structural brace-depth tracking only (nested `{`/`}` — for example ARM register lists or LLVM-style `${...}` forms — stay balanced and do not end the block early). Omega comment syntax (`#`, `##`) does not exist inside the body; `#`, `;`, `//`, register names, mnemonics, and all other punctuation belong exclusively to the target assembler's own syntax and are forwarded unchanged.

Like other block-shaped statements, `asm(...) => { ... }` needs no trailing semicolon.

## Descriptors

- **`reg(expr)`** evaluates `expr` by value and exposes the result through exactly one backend register operand, bound in the body as `$<name>` or by position.
- **`reg(expr, "phys")`** additionally requests a specific physical register (for example `"rcx"`, `"x0"`). Omega does not validate that the string names a real register for the target; the backend/assembler does.
- **`const(name)`** exposes a named `comp` binding as assembler-visible immediate text. It performs no runtime evaluation and reserves no register.
- **`clobber("phys")`** declares a physical register or piece of machine state that the body destroys without receiving a value through it. It carries no input value, no output value, and generates no instruction.

Runtime `reg` expressions are evaluated exactly once, left to right, in descriptor order. `const` and `clobber` descriptors never evaluate anything at runtime.

## Operand bindings

Inside the body, `$name` refers to the `reg`/`const` descriptor whose expression is the obvious named source (`reg(x)`, `reg(&x)`, `const(SIZE)`); `$N` refers to the zero-based position of a `reg`/`const` descriptor (never a `clobber`) among the bindable descriptors, for expressions with no usable inferred name. Omega rewrites `$name`/`$N` occurrences to whatever the backend needs — an LLVM template slot for a `reg`, or literal constant text for a `const` — without parsing the surrounding instruction.

`$$` is the source escape for one literal `$` in the final target assembly and is recognized before `$name`/`$N` scanning, so an escaped dollar is never misread as an operand binding.

The binding namespace is defined purely by source descriptor order; it is independent of however the backend numbers its own internal operands.

## Value model

- `reg(expr)` is a **by-value snapshot with no implicit writeback**. The body may freely overwrite the register that carries the value; the resulting register contents are discarded once the `asm` statement completes. To mutate Omega storage, pass its address explicitly: `reg(&mut x)`.
- `reg(&x)` and `reg(&mut x)` are equivalent asm operands; there is no backend mutability distinction between them, and neither is treated as proof that the pointee is initialized. `asm` never observes or checks the body, so it cannot verify that the body actually writes through a mutable pointer it was given.
- `reg(expr)` preserves the expression's real scalar/pointer-like machine type: no implicit cast to `usize`, no aggregate decomposition, no ABI flattening, and no memory fallback. Values that cannot occupy a single register on the selected target (aggregates, fat/multi-word language values, `void`/`never`) are rejected before code generation; final target-specific register representability (for example whether a given target has a floating-point register class at all) is a backend decision reported as an ordinary compiler error.
- `const(name)` only accepts a named `comp` binding and only when its value converts deterministically to assembler text. It is textual substitution, not a hidden runtime argument; an immediate-value prefix required by the target syntax (such as `#$NAME`) is written by the user around the binding.

## Clobbers

`clobber("...")` is required for any physical scratch register or machine state the body modifies beyond what its `reg` operands already imply. A `reg(x, "rax")` already tells the backend that the register carrying `x` may be destroyed, so a matching `clobber("rax")` for that same register is redundant. Omega does not read instruction text to infer which registers a body touches; an undeclared clobber is a violated unsafe-asm contract, not something the compiler detects.

## Side effects and optimization

Every `asm` statement is treated conservatively: side-effecting, and able to read or write arbitrary memory. Omega never inspects, folds, deletes, combines, reorders, substitutes, vectorizes, or otherwise optimizes instructions inside the body — after operand binding, the instruction text is handed to the backend as one opaque, semantically sacrosanct template. Code around the `asm` statement may still be optimized normally.

This is a deliberate trade: Omega gives up some optimization freedom in exchange for never needing to understand target instruction semantics.

## Registers versus code shape

Omega guarantees preservation of the body's **instruction template and semantics** — the actual instruction sequence the user wrote is neither reordered nor rewritten. It does not guarantee exact surrounding machine code or exact byte-for-byte encoding: the backend may still insert register copies, spills, or reloads around the `asm` statement to satisfy register allocation and the declared constraints/clobbers, and the target assembler may choose its own encodings or expand target-defined pseudo-instructions/aliases. Code requiring exact bytes must use whatever raw-data/directive facility the selected assembler provides, outside of `asm`.

## Dialect

Each architecture uses exactly one assembler dialect; Omega exposes no per-statement dialect switch. X86/X86-64 use LLVM's Intel dialect only.

## Function contract

An `asm` statement must fall through to the next Omega statement normally: it must not return, unwind, or jump into surrounding Omega control flow, and it must leave the stack pointer as it found it. Internal target-local branches/labels confined to the body are ordinary backend text and are allowed.

### Naked functions

The sole `asm` statement inside a successfully validated `@naked` function (see [`functions.md`](functions.md#naked-functions)) is the one exception to the function contract above: it *is* the entire function implementation, so it owns control flow completely and may return from, tail-jump out of, or otherwise not fall through the surrounding Omega function. A naked function's `asm` is additionally restricted to `const(...)` and `clobber(...)` descriptors -- `reg(...)` is rejected there because materializing a runtime register operand would give the backend permission/need to emit setup/teardown instructions around the body, contradicting nakedness. Every other asm rule (opaque instruction text, target dialect, `$` binding, constant substitution, no optimization/rewrite of the body) still applies unchanged.

`asm goto`-style control transfer out of an ordinary (non-naked) asm block remains a separate, unimplemented feature.

## Compile-time evaluation

`comp` evaluation cannot execute an `asm` statement; attempting to run one during compile-time evaluation is a compile-time-evaluation error, not a language-level ban. A runtime function that merely *contains* an `asm` statement remains a legal `comp`-evaluable declaration as long as evaluation never reaches the statement.

## Example

```omega
mut x : i32 = 0;
y := 20i64;
asm(reg(&mut x, "rcx"), reg(y)) => {
    add $y, 22
    mov dword ptr [$x], eax
}
```

## Rejected designs

- GCC/Rust-style `in`/`out`/`inout` operand syntax and language-level asm outputs: these imply implicit writeback, which conflicts with Omega's value/address model.
- Treating `reg` operands as implicitly `usize`-castable: breaks natural floating-point register use and discards real type information.
- Automatic instruction, clobber, or mutability analysis of the body: would require Omega to parse architecture-specific assembly and still would not match the constraint-based correctness model the backend actually relies on.
- Memory operands or automatic aggregate decomposition: out of scope for this feature; addresses are passed explicitly via `reg(&x)`/`reg(&mut x)`.
