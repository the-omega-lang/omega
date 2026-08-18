# Implementation plans

Each file here is one design/implementation plan — written and reviewed
*before* the work started, then kept as-is once the work landed. Plans are
numbered in the order they were written (`0001-`, `0002-`, ...) and named
after what they change.

A plan is a snapshot of intent, not documentation. It records what was
being asked for, the alternatives that were rejected and why, and what was
deliberately left out — the reasoning that a finished diff can't show. For
the language's *current* state, read [`docs/`](../README.md) instead; where
a plan and the docs disagree, the docs are right.

## No full history

We only started keeping plans at `0001`. Everything built before that —
most of the language — has no plan on file. Its rationale lives in the
docs, in the code's own comments, and in the commit history; nothing is
missing from this directory that ever existed here.


## Current documentation layout

Current documentation is split by responsibility:

- `docs/language/` — normative language semantics and grammar.
- `docs/architecture/` — compiler/runtime implementation design.
- `docs/guide/` — non-normative programmer guidance and examples.
- `docs/issues/` — current bugs, implementation deviations, limitations, and design debt.

Archived plans may refer to the former numbered flat documentation paths. Those references are historical and are intentionally not rewritten as if the old plan had been authored against the new layout.

## Agent use: cold storage

Coding agents should treat this directory as **historical cold storage**, not normal task context. Do not search or read old plans merely because their title or vocabulary resembles the current task. Start from [`ARCHITECTURE.md`](../../ARCHITECTURE.md), current `docs/`, and current source instead.

Consult an old plan only when current source/docs leave a specific, necessary design rationale unresolved. Read the smallest relevant plan/range, then return to current sources of truth. Historical plans must never override current code or documentation.
