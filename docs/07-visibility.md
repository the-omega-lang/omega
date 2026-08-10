# Visibility

```
exposed struct Public { ... }
internal struct PackageWide { ... }
struct HiddenByDefault { ... }          # no modifier = hidden
reveal some_module::hidden_thing()      # bypass, use-site only
```

Four levels, `Hidden < Internal < Exposed` (a real, derived `Ord` — the
ordering is meaningful, not just a derive of convenience: see the spec rule
below). `exposed`/`internal`/`reveal` are contextual keywords (recognized
by identifier text, like `mut`), not reserved words.

- **`exposed`** — visible from anywhere.
- **`internal`** — visible anywhere within the same top-level package (same
  root module path segment), regardless of nesting depth. This is Rust
  `pub(crate)`-style — explicitly **not** restricted to descendants of the
  declaring module.
- **(no modifier, the default)** — the narrowest level, `Hidden`, and it
  means two different things depending on what it's attached to (see
  below).
- **`reveal`** — a use-site prefix that bypasses whatever visibility
  restriction would otherwise apply, at both expressions (`reveal
  base.method()`, `reveal Struct { f = v }`) and imports (`import reveal
  extern::lib;`). Contagious in the sense that it wraps arbitrary
  expressions, not just place chains; warns (`UnnecessaryReveal`) if the
  bypass turns out not to have been load-bearing.

## Hidden items vs. hidden members — two different scopes, same default

This is the single most important, easy-to-get-wrong distinction in the
whole system, and it was fixed in two separate rounds after two separate
user-reported regressions of the same underlying pattern:

- **A hidden top-level item** (struct/enum/union/spec/function/global) is
  visible within the exact declaring **module** — any code in that file,
  including an unrelated free function, can see it.
- **A hidden field or method** is visible only within the exact declaring
  struct/union/enum's **own method bodies** — strictly narrower than
  module scope. A free function in the same file as `struct S {
  print_addr(*self) { ... } }` (no modifier — hidden by default) cannot
  call `s.print_addr()`, even though it could freely reference another
  hidden top-level item in that same file.

Enforced via `Analyzer::current_owner: Option<HirId>` (set once, at the top
of a struct/union/enum's own body-check, or a pending spec-method check) —
`Hidden` member visibility checks `current_owner == Some(declaring
type's id)` instead of a module-path comparison. `Internal`/`Exposed`
members still use the ordinary module/package check; only `Hidden`'s
scope differs between an item and a member.

## `reveal`'s real complexity

`reveal` is a genuine, transparent expression-wrapper AST/HIR node
(`Expression::Reveal`), not folded into the place-chain machinery
(`FieldAccess`/`Index`/`Deref` already share), because it has to wrap
non-place expressions too (`reveal SomeStruct { f = v }`, `reveal
foo(a, b)`).

**The subtlety that actually caused a shipped bug**: whether `reveal`'s own
analysis arm (which pushes/pops a bypass frame) *runs at all* depends on
where the `Reveal` node ends up relative to its enclosing expression once
parsed:

- `reveal base.method()` — `Reveal` wraps the *whole call*, since `()`  is
  consumed as part of the same unary/postfix chain `reveal` sits in. The
  bypass frame is active by the time method resolution runs. Works as
  expected.
- `reveal abc.number = 10;` — `Reveal` wraps *only* the assignment's
  target; `=` is handled one level up, entirely outside the chain `reveal`
  occupies. The outer node is `Assignment`, not `Reveal` — so a naive
  "only fire the bypass when `Reveal` is the outermost node" implementation
  never activates the bypass for this position at all, despite this being
  the user's own primary documented example for the feature.

Fixed by explicitly re-checking for a stripped `Reveal` wrapper (and
activating its bypass) at every genuine target/operand position that isn't
itself call/postfix syntax: plain assignment, compound assignment,
`++`/`--`, and `&`/`&mut`.

That list has since grown twice more, both times for the same reason: `&`'s
own two non-place operand shapes, `&reveal base[range]` (a slice) and
`&reveal [...]` (a compile-time slice), each returned early *before* the
bypass was activated, so `reveal` silently did nothing there. Both now run
under the same bypass as the plain-place form.

The invariant itself is still "every position must remember", with no
compiler-enforced backstop — see
[design-review.md](17-design-review.md#compiler-architecture) for why the
structural fix is to make place resolution own the bypass instead.

## Specs: inherited visibility + minimum-permissiveness

A spec function has **no visibility modifier of its own** — it inherits
its *declaring* spec's own visibility. An implementor's satisfying method
must be **at least as permissive**:

```
internal spec Mammal : Animal { breathe(*self) => i32; }

struct Dog : Mammal {
    internal breathe(*self) => i32 { ... }    # OK: internal >= internal
}
struct Cat : Mammal {
    breathe(*self) => i32 { ... }             # ERROR: SpecMethodTooHidden
}                                               #   (hidden < internal)
struct Wolf : Mammal {
    exposed breathe(*self) => i32 { ... }     # OK: exposed >= internal
}
```

This is a purely structural, declaration-time rule (checked once, in
`resolve_implements_clause`) — `reveal` has no bearing on it at all, the
same way `reveal` doesn't apply to the equally-structural
`MissingSpecFunction` check. Each function's threshold comes from *its own
declaring spec*, independent of what's at the top of an `implements`
clause: `spec Mammal : Animal` where `Animal` is hidden and `Mammal` is
`exposed` still only requires `hidden` for `Animal`'s own functions.

**Why this is also checked at dynamic-dispatch coercion time, not just
declaration time**: naively, "the implementor already satisfies the
ordinal rule at declaration" seems sufficient — anyone who can see the spec
is already inside its implementing methods' audience, since a struct can
only write `: Spec` for a spec it can already resolve. This genuinely held
**for `Internal`/`Exposed`**, but broke specifically for `Hidden` the
moment hidden *methods* became owner-scoped (narrower than a hidden
*item*'s module scope, see above): `{method bodies of S} ⊂ {all code in
S's module}` is a *strict* subset, so a hidden method's real audience is
narrower than what the ordinal-only proof assumed.

Concretely, this was exploitable: a free function that's correctly denied
`foo.say()` directly could still do `s : spec *Talker = &foo; s.say();`
with no error at all — dynamic dispatch was a strictly wider hole than the
direct-call rule it was meant to mirror. Fixed by re-checking each
flattened requirement's matched method's visibility specifically at the
one place a concrete method identity is erased into an opaque `spec *T`
handle (`coerce_to_expected`) — *not* at generic-bound checking
(`check_generic_bound`), since a `T: Animal` bound is a structural fact
about `T`, and a generic body's own `self.speak()`-style calls are already,
correctly, checked individually at their own real call sites.

## Caveats

- **No re-export / `pub use`-equivalent.** `import reveal lib::x;` only
  lets *this* module's own references bypass `x`'s visibility — it doesn't
  change what a third module sees through this module's own alias. Matches
  the language having no re-export concept at all today.
- **A named import alias's overload candidate set is fixed at import
  time**, deliberately not reachable by a later call-site `reveal`: `import
  lib::pick;` (no `reveal`) permanently excludes any overload of `pick`
  this module can't see from the candidate set — a call whose arguments
  only match an excluded overload is a hard `NoMatchingOverload`, as if
  that overload didn't exist. Only `import reveal lib::pick;` brings every
  overload into context (with no call-site `reveal` needed afterward). A
  module-qualified reference through a *whole*-module import (`lib::pick(...)`
  via plain `import lib;`) is explicitly exempt from this restriction —
  every overload is always a candidate there, and call-site `reveal` still
  works normally, since there's no per-symbol "import reveal" granularity
  that could even apply to a whole-module import.
- Build reproducibility (several `HashMap` iteration sites making object
  files differ build-to-build for identical source) was discovered
  incidentally while verifying a visibility change, but is unrelated to
  visibility itself and has since been fixed — see
  [modules & linkage](10-modules-and-linkage.md).

Macros are ordinary visibility-bearing items: an unmodified macro is
file-local, `internal macro` is package-visible, and `exposed macro` is
visible to importers and the ambient `core` prelude.
