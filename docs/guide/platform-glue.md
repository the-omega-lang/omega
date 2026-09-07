# `plat`: the platform packages

`runtime/plat/` — a plain directory, not a package itself. It holds reusable
implementation fragments and, under `target/`, one directory per concrete
platform. Exactly one of those target directories is compiled per build, and
it *presents* as the declared identity `plat` purely via a compiler-level
alias (`plat:<dir>`/`--import=plat:...`) — the project's own files never lie
about what they are; only the compiler's view of a root's identity can differ
from its on-disk name. Unlike `core` ([the core library](core-library.md)),
`plat` gets no ambient-prelude treatment, no `primitive`-block privilege, and
no eager-discovery exemption of its own; it's just an ordinary `--import`
package that happens to ship `glue` implementations for `core`'s own gaps (see
[gaps and glue](../language/gaps-and-glue.md)). Any consumer that registers it
gets its glue discovered automatically, whether or not it ever `import`s
`plat` itself — the same eager, whole-program struct/spec surface resolution
every registered extern gets (see
[modules & linkage](../language/modules-and-imports.md)'s "Eager local
discovery").

None of these platforms requires a libc. The four hosted ones talk to the
operating system directly, and the freestanding one talks to nothing at all.

## Layout

```text
runtime/plat/
  arch/                     # architecture-only implementation
    x86_64/atomic.omg
    aarch64/atomic.omg
    avr/atomic.omg
  os/                       # operating-system implementation
    linux/
      common/               # Linux policy shared by every architecture
        allocator.omg
        io.omg
        process.omg
      arch/                 # the OS x architecture intersection
        x86_64/{syscall,entry}.omg
        aarch64/{syscall,entry}.omg
    windows/                # one Win32 surface for both architectures
      {api,allocator,io,process,entry}.omg
  common/
    hosted/panic.omg        # panic policy every hosted target shares
  libc/
    libc.omg                # explicit compatibility platform
  target/                   # the roots that are actually compiled
    x86_64-linux/
    aarch64-linux/
    x86_64-windows/
    aarch64-windows/
    avr-none/
```

### Composition

A target root contains no implementation of its own — except where the target
really does own something, as `avr-none/panic.omg` does. It is built from
**committed relative symlinks** into the fragments above it:

```text
runtime/plat/target/x86_64-linux/
  arch          -> ../../arch/x86_64
  common        -> ../../common/hosted
  os/                                    # a real directory: this is the
    allocator.omg -> ../../../os/linux/common/allocator.omg
    io.omg        -> ../../../os/linux/common/io.omg
    process.omg   -> ../../../os/linux/common/process.omg
    syscall.omg   -> ../../../os/linux/arch/x86_64/syscall.omg
    entry.omg     -> ../../../os/linux/arch/x86_64/entry.omg
```

The compiler needs no new mechanism for this. `omgc` discovers modules through
the filesystem and keeps each file's path relative to the selected root, so
the package it sees is an ordinary one with modules `plat::arch::atomic`,
`plat::common::panic`, `plat::os::io` and so on — whatever the files are
symlinks to. There is no target `cfg`, no compiler-side platform selection and
no generated copy of the tree.

The Windows roots link `os` as a whole directory, because Windows uses one
Win32 surface on both architectures. The Linux roots link `os` file by file,
because the syscall ABI and the process entry sequence differ per architecture
while allocator, console and termination policy do not. That is the one case
the file-level form exists for.

`bin/check-plat-links` validates the composition, and `just build-runtime-for`
runs it first: a checkout that materialized the symlinks as plain text files
would otherwise compile a platform with no allocator, no console and no entry
point instead of failing.

## Ownership rule

Put an implementation at the narrowest layer that is actually true of it:

| Layer | Owns | Example |
|---|---|---|
| `arch/<arch>/` | what only the instruction set decides | atomics |
| `os/<os>/common/` | what the OS decides the same way everywhere | Linux allocator, console, termination policy |
| `os/<os>/arch/<arch>/` | the OS x architecture intersection | Linux syscall numbers/registers, the ELF entry sequence |
| `common/<class>/` | policy shared by a class of targets, expressed only through gaps | the hosted panic message and its `StandardError` output |
| `target/<target>/` | composition, plus anything genuinely specific to that target | `avr-none`'s panic |

Do not force a fact into a purer-looking layer than it belongs to. Linux's
`write` number is not an architecture fact and not an OS-only fact; it is both,
and it lives in the intersection.

## Capability matrix

| | x86_64-linux | aarch64-linux | x86_64-windows | aarch64-windows | avr-none |
|---|---|---|---|---|---|
| `GlobalAllocator` | ✓ | ✓ | ✓ | ✓ | — |
| `StandardOutput`/`Error`/`Input` | ✓ | ✓ | ✓ | ✓ | — |
| `PanicHandler` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `Atomicity8/16/32/64` | ✓ | ✓ | ✓ | ✓ | ✓ |
| process entry point | `_start` | `_start` | `omg_start` | `omg_start` | — |

A blank is a capability the target does not have an honest answer for, not an
oversight. Referencing one is a link error naming the missing glue symbol,
which is the intended outcome: nothing here stubs a capability out with a
`None` or a null so a program links and then misbehaves.

## What each platform does

### Linux (`x86_64-linux`, `aarch64-linux`)

- **Entry** — `os/entry.omg` defines the ELF entry symbol `_start` as a
  `@naked` function. The kernel does not enter it like a called function: the
  initial stack holds `argc`, `argv`, `envp` and the auxiliary vector and there
  is no return address, so an ordinary Omega prologue would misread it. It
  calls `_omg_main` (the fixed symbol Omega emits for the root `main`, see
  [the FFI chapter](../language/foreign-function-interface.md)) and, if that
  returns, ends the process with status 0.
- **Syscalls** — `os/syscall.omg` is the only module that knows a syscall
  number, the trap instruction's register ABI, or the kernel's negative-errno
  convention. It exposes read, write, anonymous mapping, unmapping and process
  termination as normalized operations, so the shared Linux modules above it
  never repeat a number or a register name.
- **Allocator** — anonymous private mappings, one per allocation, with the
  mapping length recorded in a 16-byte header immediately before the block so
  `free` can unmap without a size argument. `realloc` allocates the
  replacement first, copies `min(old, new)` bytes and then releases the
  original, so a failed `realloc` returns null with the old block intact. The
  header keeps the returned block 16-byte aligned because a mapping starts
  page-aligned. One mapping per allocation is the visible trade: `free` needs
  no free list and no shared bookkeeping, but a small allocation still costs a
  page.
- **Console** — `read`/`write` on descriptors 0, 1 and 2. A kernel error is
  `None`; every non-negative result, including zero and a short transfer, is
  `Some(count)`. Nothing retries: retry policy belongs to the caller.
- **Panic** — the shared hosted handler (below), then `exit_group(134)`. 134
  is what a shell reports for a process killed by `SIGABRT`, which is what
  this platform produced through libc's `abort` before; the status is
  reproduced deliberately, without claiming `abort`'s signal or core-dump
  behavior.

### Windows (`x86_64-windows`, `aarch64-windows`)

- **API surface** — `os/api.omg` declares eight documented Kernel32 entry
  points (`GetProcessHeap`, `HeapAlloc`, `HeapReAlloc`, `HeapFree`,
  `GetStdHandle`, `ReadFile`, `WriteFile`, `ExitProcess`) and nothing else.
  These are operating-system dependencies, not C-runtime ones. Raw NT syscall
  numbers are deliberately not used: Microsoft does not keep them stable.
- **One source for both architectures** — every declaration is scalar or
  pointer shaped, so `foreign(c)` resolves to whichever machine convention the
  selected target uses, and `WINAPI` is that convention on every 64-bit
  Windows target.
- **Allocator** — the process heap, with no flags. The heap belongs to the
  process rather than to a C runtime, so it is valid from the program's first
  instruction. `HeapReAlloc` already leaves the original block valid when it
  fails, which is exactly the `realloc` contract.
- **Console** — `ReadFile`/`WriteFile` on the standard handles, fetched per
  call so a `SetStdHandle` redirection is observed immediately, and not the
  console-only APIs, so a redirected file or pipe works too. Win32 transfer
  counts are `DWORD`, so a longer slice is offered one `DWORD`'s worth and the
  short result is reported as the ordinary `Some(count)` the gap already
  allows. A null or invalid handle, or an API failure, is `None`.
- **Entry** — `omg_start`, an ordinary C-convention function; Windows calls
  the entry point with a normal stack frame already established. It is
  deliberately not named `mainCRTStartup`: naming it after the Microsoft C
  runtime would claim a contract this platform does not implement.
- **Panic** — the shared hosted handler, then `ExitProcess(134)`, matching the
  Linux status.

Linking a Windows executable is that toolchain's job — `omgc` only emits
objects. The link needs the Kernel32 import library and must select this
entry point:

```text
lld-link /entry:omg_start /subsystem:console /nodefaultlib \
    <core, plat, std and application objects> kernel32.lib
```

### `avr-none`

`avr-none` is generic AVR: it names no MCU. That is why it is deliberately
partial.

- **Atomics** — all four widths, by masking interrupts around an ordinary
  access. A generic AVR part is single-core, so an interrupt is the only thing
  that can interleave; masking it makes the access indivisible with respect to
  every execution context that exists. The critical section saves and restores
  the global interrupt-enable bit rather than doing a bare `cli`/`sei`, so code
  that already had interrupts disabled does not get them silently re-enabled.
  This is not lock-free and is not claimed to be.
- **Panic** — stops the part: interrupts off, then an endless loop, leaving
  the state visible to a debugger. There is no console to report on, and a gap
  takes exactly one glue, so there is no way to write "use the console if this
  build happens to have one".
- **No allocator, console, or entry point.** Each of those needs a concrete
  MCU and board: a RAM region and startup policy, a USART instance with a
  register map and a clock/baud choice, a reset vector and a vector table.
  Inventing them for a target that names no part would be a lie that links.

A future `atmega328p`-style platform is an ordinary new target root. It
composes `arch/avr` the same way, adds its own startup, serial and allocator
modules, and may leave out this generic panic file in favour of one that
reports over its serial port. No compiler change is involved.

## The `plat` alias

`<name>:<dir>` as the compiled entry argument (standalone compilation) and
`--import=<name>:<dir>` (consumption) let a package's *declared* identity
differ from its root directory's own basename. This is what lets
`runtime/plat/target/x86_64-linux/` — a directory whose name is not even a
legal module identifier — present as the package `plat`. The alias applies to
*everything* discovered beneath the root, not just the root segment itself.

A target root has no root-module source file of its own; a directory that only
groups children declares no module and produces no artifact, so the package
starts at `plat::arch`, `plat::os` and `plat::common`.

## No platform selection

There is no OS/target conditional-compilation mechanism anywhere in this
compiler (no `cfg`-equivalent), and there is no compiler-side platform
selection either. Picking a platform is entirely a build decision: which
directory `--import=plat:...` points at. The `--target` argument and the
platform root must agree, and every package of one build — `core`, `plat`,
`std` and the application — must be compiled for the same target, since it
fixes `usize`, layout and the selected platform together.

## The libc compatibility platform

`runtime/plat/libc/` is still here, still standalone-compilable as `plat`, for
builds that want to sit on a C runtime — an existing C application embedding
Omega, or a host where linking libc is simply easier than not. It supplies the
allocator, the three console capabilities and a panic handler over
`malloc`/`free`/`realloc`, `read`/`write` and `abort`, and it owns the
libc-facing `main` adapter. It fills none of the `AtomicityN` gaps: no honest
libc-only implementation of that contract exists without architecture-specific
code or a new runtime dependency such as libatomic.

It is not the default and nothing in the normal build or test path selects it:

```text
just build-plat-libc     # target/x86_64-linux/plat-libc
```

## Building

```text
just build-runtime           # core + plat + std for the host target
just build-runtime-for aarch64-linux
just build-runtime-all       # all five targets
just check-platform          # the matrix, plus the checks in checks/
```

Artifacts live under `target/<target>/{core,plat,std}` so one target's `.o`
cannot overwrite another's `.obj` at the same relative path.

The native link uses this platform's own `_start`, so it must not pull in a
CRT startup object:

```text
cc -nostdlib -static -no-pie -Wl,--gc-sections <objects> -o program
```

`-nostdlib` because there is no libc and no CRT to link, and `-no-pie` because
`_start` does not self-relocate. `--gc-sections` because a runtime object that
mentions a capability the program never uses must not retain its glue —
`just playground` and `bin/test-runner` both link this way.

Each console capability has its own glue declaration, so an application that
does not reach a console marker need not retain that glue at final link.
