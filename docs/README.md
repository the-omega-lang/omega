# Omega documentation

Omega's documentation is split by responsibility so that humans and agents can load the smallest source of truth that answers the question.

## Where to look

| Need | Read |
|---|---|
| Write or read ordinary Omega source quickly | [`guide/quick-reference.md`](guide/quick-reference.md) |
| Know exactly what an Omega program means | [`language/`](language/) |
| Understand how this compiler implements that behavior | [`architecture/`](architecture/) |
| Check known bugs, unsupported cases, or design debt | [`issues/`](issues/) |
| Understand the runtime/core/std surface | [`guide/`](guide/) |
| Recover historical implementation rationale | [`plan/`](plan/) only when current docs/source are insufficient |

## Authority model

### Language behavior

[`language/`](language/) is the **Omega Language Specification** for the language implemented by this repository. A reimplementation should reproduce the behavior described there unless a relevant entry under [`issues/`](issues/) explicitly records a known implementation deviation.

When evidence disagrees:

1. `docs/language/` defines the intended/current language semantics.
2. `docs/issues/` records known places where the current compiler or libraries do not yet meet that definition, or where behavior is intentionally incomplete.
3. Conformance tests and examples are executable evidence of behavior.
4. Compiler source is authoritative for what the current implementation actually does, but an implementation bug does not silently redefine the language.
5. `docs/guide/` is explanatory and example-oriented; it must not override `docs/language/`.
6. `docs/plan/` and git history are historical, never current normative authority.

If the specification and implementation disagree and no issue records the mismatch, investigate the contradiction instead of choosing one silently.

### Compiler architecture

[`architecture/`](architecture/) describes intended compiler/runtime structure and implementation invariants. The root [`../ARCHITECTURE.md`](../ARCHITECTURE.md) is the compact navigation map; this directory contains deeper implementation notes. Source code remains authoritative for exact current implementation details.

## Terminology

Omega already uses the keyword **`spec`** for its interface-like language construct. To avoid ambiguity, this repository uses:

- **Omega Language Specification** for the normative documentation as a whole;
- `docs/language/` for its filesystem location;
- **`spec`** (lowercase/code) only for the Omega language construct.

## Agent guidance

The old numbered-doc reading order no longer exists. Agents should not read this tree sequentially.

- When generating or modifying `.omg`, start with `guide/quick-reference.md`.
- For exact semantics, open only the relevant file(s) in `language/`.
- For compiler implementation questions, use the root `ARCHITECTURE.md`, then relevant `architecture/` docs/source.
- Consult `issues/` only when working in the affected area or when observed behavior conflicts with the language/architecture documentation.
- Treat `plan/` as cold storage.
