# Console I/O

`core::io` provides explicit, allocation-free `Writer` and `Reader` values. A
writer is unbuffered by default (`Writer::stdout()` / `stderr()`); bytes reach
the platform sink before the call returns. `stdout_buffered`, `stderr_buffered`,
and `to_sink` use a caller-owned buffer and require an explicit `flush`. A
memory-only `to_buffer` writer sets `had_error()` when full rather than writing
past the supplied storage.

`Reader::stdin()` is similarly unbuffered. `stdin_buffered` accepts caller
storage. `read` and `read_line` use an out-count plus `bool`; successful zero
bytes is EOF. Fixed-buffer `read_line` rejects a non-fitting line instead of
silently reporting a truncation.

## Platform gaps

The only platform-specific contract is three independent gaps:
`StandardOutput::write`, `StandardError::write`, and `StandardInput::read`.
They use an out-count and `bool`, so targets can implement only the console
capabilities they possess. The hosted libc package fills each one with its own
`glue` block, implemented with `write(2)` and `read(2)`. `StandardOutput` and
`StandardError` deliberately declare the *same* `write` signature: a `glue`
block has no type of its own, so two identically named functions in separate
blocks never collide — see [gaps-and-glue.md](21-gaps-and-glue.md).

## Formatting and printing

`core::fmt::Display` writes a value to a `Writer`. Integers, floats, `bool`,
`char`, and `str` implement it. Integer digit conversion has bases 2–16.
Floats use six fixed fractional digits; `nan`, `inf`, and `-inf` are handled,
and values below `1e-6` or above `1e19` use scientific notation. This is
intentionally not a shortest-round-trip formatter.

`print$`, `println$`, `eprint$`, and `eprintln$` are exposed `core` macros.
They visibly allocate a 256-byte stack buffer in their expansion, format each
argument through `Display`, then flush. They are concatenative:
`println$("count=", count)`, not format-string based. One syscall per print
statement, no global state, and nothing left pending when the statement ends.

Their locals are named `omega_print_buf`/`omega_print_out` rather than
`buf`/`out`. Omega has no macro hygiene and a statement-position expansion is
spliced into the caller's own block, so a plainer name is *captured* by any
argument expression referencing a caller variable of the same name — see
[macros.md](12-macros.md)'s hygiene section. The surrounding `{ }` keeps them
from leaking back out.

`examples/dev/main.omg` — the language's main integration example, ~150 print
sites — was migrated off libc `printf`/`puts` onto these macros and declares no
`extern` of its own; `nm -u target/main.o` shows no `printf`/`puts` reference.
Two output differences fell out of the switch, both corrections: a `bool` now
prints `true`/`false` rather than the `%d` fallback `1`/`0`, and a `char`
prints as its character rather than its codepoint. Addresses are formatted by
dropping to the `Writer` API directly (`write_uint(&mut w, addr, 16u32)`),
since the macros carry no format specifiers by design.

NUL-terminated `*u8` text went with it. Every `<*u8>b"...\0"` literal in that
example existed only because `puts` scans for a terminator and `printf`'s `%s`
does too; all 27 are now plain `*str` literals, and the signatures that carried
them (`classify`, `make_sound`, `print_any`, `Interface::do_thing`, the
`some_text`/`favorite_color`/`message` fields) say `*str`. A length-carrying
fat pointer is what `core::io` wants anyway: printing one is a single `Display`
call with no cast, no terminator scan, and no way to read past the end — where
`printf` needed `%.*s` plus a `<*u8>` length-drop to be sound at all. The `b"…"`
literals that remain are the two that genuinely demonstrate byte slices
(`*[?]u8` and the `ToIterator<u8>` example), not text.

## `std::io`

`std::io::string_writer` targets an owned `String`; `read_line` grows a
`String`; `to_string<T: Display>` owns its result. Build hosted programs with
`just build-core`, `just build-std`, `just build-plat`, and `just test-io`.

## Caveats

There is no implicit flush at program exit. A caller-owned buffered writer
must be flushed. Macro bodies resolve nested macro calls at the invocation
site.

A user-defined type implements `Display` with
`compose Pair : Display { fmt(*self, out: *mut Writer) => void { ... } }` —
see `examples/io_demo/main.omg`. Primitive inherent methods live in core-only
`primitive` blocks; their `Display` conformances are separate compose blocks.
For any compose, either the target type or the spec must belong to the current
package.

The four print macros expand each argument to `Display::fmt($args, ...)`, the
spec-qualified form, not `($args).fmt(...)`: a composed method is only
reachable through its spec, and the expansion site has no bound to reach it
through. `Display` needs no import at the call site — an unqualified spec name
resolves ambiently from `core`, the same way the rest of core's prelude does.
The receiver is adapted to `fmt`'s `*self` exactly as a method call would
adapt it, so an argument that is a literal or a temporary works unchanged.
