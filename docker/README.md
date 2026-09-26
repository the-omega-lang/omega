# Omega development container

A reproducible Alpine Linux environment for developing Omega, with Claude Code,
Codex CLI, omp (oh-my-pi) and opencode installed on first use and persistent
across runs.

You need Docker (with the Compose plugin) and nothing else — no Rust, no
`just`, no Node on the host.

## Quick start

```sh
./dev.sh            # builds the image on first run, then starts Claude Code
./dev.sh codex      # same, but starts Codex CLI instead
./dev.sh omp        # same, but starts omp
./dev.sh opencode   # same, but starts opencode
```

The first run downloads the base image and the Rust toolchain (a few minutes),
then installs whichever agent you asked for. Every run after that starts in a
second or two. Agents are installed one at a time, on demand, so `./dev.sh run
just test-all` on a fresh machine downloads none of them.

Log in to each tool once, inside the container; the login is stored in a
Docker volume and reused by every later run.

## Commands

| Command | What it does |
| --- | --- |
| `./dev.sh` | Start Claude Code in the container |
| `./dev.sh codex` | Start Codex CLI in the container |
| `./dev.sh omp` | Start omp (oh-my-pi) in the container |
| `./dev.sh opencode` | Start opencode in the container |
| `./dev.sh shell` | Interactive bash shell in the container |
| `./dev.sh run <cmd...>` | Run a single command, e.g. `./dev.sh run cargo test` |
| `./dev.sh update-agents [agent...]` | Update the agents to their current release, no rebuild |
| `./dev.sh build` | Build the image if missing or out of date |
| `./dev.sh rebuild` | Rebuild from scratch — the Rust/LLVM toolchain, not the agents |
| `./dev.sh down` | Remove leftover containers, keep all volumes |
| `./dev.sh clean` | Remove containers **and volumes** (caches, history, agents and their logins) |
| `./dev.sh help` | Usage |

Inside the container the project's own workflow works unchanged:

```sh
./dev.sh run just build-exe
./dev.sh run just run-exec
./dev.sh run cargo clippy --workspace
```

## What is in the image

Built from `alpine:3.23`:

- **Rust 1.94.1** via `rustup` (`x86_64-unknown-linux-musl`), plus `rustfmt`
  and `clippy`. Pinned by build argument, not by whatever Alpine ships.
- **build-base / binutils / gdb** — `cc`, `as` and `ld`, which the `justfile`
  and `bin/test-runner` invoke directly to link the object files `omgc`
  emits, and to build the freestanding C helpers a few tests ship.
- **LLVM 21** (`llvm21-dev`, `llvm21-static`, `clang21`, `lld21`) — the
  `omega-codegen` backend. `llvm-config` and the rest
  of the LLVM tools live under `/usr/lib/llvm21/bin`, which is on `PATH`, so
  `llvm-sys` finds them without an `LLVM_SYS_<ver>_PREFIX` variable. The
  static archives are not optional here: this toolchain is
  `x86_64-unknown-linux-musl`, where Rust enables `crt-static` by default, so
  LLVM gets linked statically either way. `clang` and `lld` come along as a
  cross-assembler and cross-linker — one binary each for every target, where
  binutils needs one build per target. This is much the largest thing in the
  image: `/usr/lib/llvm21` alone measures 674 MB, around 700 MB with the
  static system libraries.
- **just** — the project's task runner.
- **ripgrep** from apk, which Claude Code uses as its search backend.
- **GNU userland** (`coreutils`, `findutils`, `grep`, `sed`, `diffutils`)
  instead of busybox's reduced applets, so shell commands behave the way
  tooling expects.
- A non-root `dev` user created with **your** uid/gid, so files the container
  writes into the repo are owned by you.

What is *not* in the image: the four agent CLIs. See the next section.

## The agents

Claude Code, Codex, omp and opencode are not image content. They are installed
into Docker volumes by `docker/install-agents.sh`, which the entrypoint runs
for an agent the first time you ask for that agent, and which
`./dev.sh update-agents` runs on demand afterwards.

That script is not copied into the image either — it runs from the repo at
`/workspace/docker/`, which compose always bind-mounts. So editing how an agent
installs, or changing which version it tracks, takes effect on the next run
with no rebuild, exactly like `bin/test-runner` or the `justfile`.

They were image layers once. Three things were wrong with that:

- **Size.** Codex 370 MB, omp 214 MB, Claude Code 213 MB, opencode 187 MB —
  982 MB between them, more than LLVM, and the largest thing in the image by a
  distance. Every `./dev.sh rebuild` re-downloaded all of it.
- **The pin was not a pin.** The defaults were `stable`, `latest`, `latest`,
  `latest` — floating channels. Nothing was pinned to a version; what froze
  the agents was the Docker layer cache, so you got whatever was current
  whenever you last rebuilt, and a plain `./dev.sh build` could never move
  them because the build argument had not changed.
- **The update path cost a rebuild** of Rust and LLVM to move a binary that
  ships several times a week.

Reproducibility of the *build toolchain* is what matters for a compiler — Rust,
LLVM, binutils, Alpine decide what `omgc` emits and whether a test failure means
anything. Which release of Claude Code you type at does not. So the pins stayed
where they earn their keep and the agents moved out.

What each installer does, since the details are load-bearing:

- **Claude Code**, from `https://claude.ai/install.sh`. On musl it lays down a
  self-contained executable whose only dynamic dependency is musl libc itself
  — no Node.js runtime, no glibc shims. `~/.local/bin/claude` is a symlink
  into `~/.local/share/claude/versions/`.
- **Codex CLI**, from `https://chatgpt.com/codex/install.sh` — also a
  self-contained musl binary, no Node.js involved. `~/.local/bin/codex` is a
  symlink; the payload goes under `$CODEX_HOME/packages`, which is its own
  volume.
- **omp (oh-my-pi)**, from `https://omp.sh/install` with `--binary`, which
  fetches the prebuilt `linux-musl` release into `~/.local/bin`. The flag
  matters: without it the installer prefers building from source through Bun,
  and installs Bun itself to do so. The musl build links `libstdc++`/`libgcc`
  dynamically, so both are in the image's apk list.
- **opencode**, from `https://opencode.ai/install`. Its installer detects musl
  itself — it checks for `/etc/alpine-release`, then falls back to
  `ldd --version` — and fetches the `-musl` release asset, another
  self-contained binary with no Node or Bun behind it. It is the one tool that
  installs outside `~/.local/bin`: its prefix is `~/.opencode/bin`, which is
  why that directory is on `PATH` in the Dockerfile.

Both installer scripts that read their settings from the environment are
fetched to a file and then run, rather than piped. `VAR=x curl ... | sh` puts
`VAR` on `curl` — the first command of the pipeline — where the installer never
sees it.

### Choosing a version

By default each agent tracks its newest release, so
`./dev.sh update-agents` moves all four to current. To hold or roll back a
release, set that agent's variable — it is read at install time, not build
time:

```sh
CLAUDE_CODE_VERSION=2.1.220 ./dev.sh update-agents claude
CODEX_VERSION=0.51.0        ./dev.sh update-agents codex
OMP_VERSION=v17.2.12        ./dev.sh update-agents omp
OPENCODE_VERSION=0.4.2      ./dev.sh update-agents opencode
```

`CLAUDE_CODE_VERSION` takes whatever `claude install` takes: `stable`, `latest`
(the default here) or an exact version. Note that the installer's own `stable`
is a conservative rollout channel that can trail `latest` by a week or more, so
this defaults to `latest` instead; set `CLAUDE_CODE_VERSION=stable` if you
would rather lag deliberately. `CODEX_VERSION` takes what Codex's installer takes:
`latest` (there is no `stable` channel) or an exact version. `OMP_VERSION` is
`latest` or an exact release tag — it is passed as `--ref`, so it carries the
leading `v`. `OPENCODE_VERSION` is `latest` or an exact version, passed through
the `VERSION` variable its own installer reads; unlike omp's it carries no
leading `v`. None of these installers fails on its own when it cannot find the
release you named, so `install-agents.sh` checks the result and fails loudly
instead.

Export the variable from your shell profile to make a pin permanent; a
first-use install honours it too.

In-place auto-updaters stay disabled (`DISABLE_AUTOUPDATER=1`,
`CODEX_UPDATE_DISABLED=1`, `OPENCODE_DISABLE_AUTOUPDATE=1`). The install
prefixes persist now, so self-updating would work — but it would mean four
tools updating on four schedules, each adding start-up latency, and a binary
being rewritten underneath a session already running it, since several
containers share these volumes at once. One explicit update path is simpler.
That is the model omp has always had: it only moves when you run `omp update`
yourself, which is why it is the one agent that never needed a switch.

## Reproducibility

Every version of the build toolchain is a pinned build argument in
`docker/Dockerfile`, overridable from the environment:

```sh
RUST_VERSION=1.95.0 ./dev.sh rebuild
ALPINE_VERSION=3.24 ./dev.sh rebuild
LLVM_VERSION=20 ./dev.sh rebuild
```

These are exact versions, and they are the ones that decide what `omgc` emits.
The agents have no build argument at all — see
[Choosing a version](#choosing-a-version).

`LLVM_VERSION` is the odd one out: it is a bare *major* version, and it is
pinned from two directions rather than one. It has to be a major Alpine
packages — 3.23 carries 16, 20 and 21 — and simultaneously one the Rust
bindings target, which is why it defaults to 21: that is Alpine's own default
`llvm` meta package, and what both `llvm-sys` 211.x and inkwell's `llvm21-1`
feature are built for. Changing it moves `PATH` and the apk package names
together, so nothing else in the image needs touching, but the Rust-side
version selection has to move with it.

If a `rust-toolchain.toml` is ever added to the repo, rustup honours it inside
the container too, and it takes precedence over `RUST_VERSION`.

## CPU limit

By default a session may use **80% of the CPUs Docker can see**, so a full
`cargo build` or a busy agent still leaves the host responsive. Change the
share with `OMEGA_CPU_PERCENT`:

```sh
OMEGA_CPU_PERCENT=50 ./dev.sh       # half the machine
OMEGA_CPU_PERCENT=100 ./dev.sh      # everything
OMEGA_CPU_PERCENT=off ./dev.sh      # no limit at all
```

`./dev.sh` turns the percentage into the absolute core count compose wants
(`cpus:` in `docker/compose.yaml`) — on a 24-core machine the default becomes
`19.20`. It is a ceiling on total CPU time, not a pin to particular cores, so
the container still spreads its work over every core, just never more than
that many cores' worth at once. Set it per run as above, or export it from
your shell profile to make it permanent.

Two things the limit does not cover: `./dev.sh build` and `./dev.sh rebuild`,
which run through BuildKit rather than this service, and sessions already
running — each container takes the value it was started with, and several
sessions at once each get their own share.

## What persists, and what does not

Persisted in named volumes (survive `./dev.sh down`, container restarts and
image rebuilds; removed only by `./dev.sh clean`). `build` and `rebuild` are
image-only operations and never touch a volume, which is why the agents stay
put across a rebuild:

| Volume | Mounted at | Contents |
| --- | --- | --- |
| `agents` | `/home/dev/.local` | The `claude`, `codex` and `omp` binaries |
| `opencode-bin` | `/home/dev/.opencode` | The `opencode` binary |
| `claude-config` | `/home/dev/.claude` | Claude Code login, settings, session history, todos |
| `codex-config` | `/home/dev/.codex` | Codex CLI login, settings, session state, and its binary payload |
| `omp-config` | `/home/dev/.omp` | omp login, settings, session transcripts, blob store, memory |
| `opencode-config` | `/home/dev/.config/opencode` | opencode settings (`opencode.json`, `tui.json`) |
| `opencode-data` | `/home/dev/.local/share/opencode` | opencode credentials (`auth.json`) and session state |
| `cargo-registry` | `/usr/local/cargo/registry` | crates.io downloads |
| `cargo-git` | `/usr/local/cargo/git` | git dependency checkouts |
| `target` | `/workspace/target` | Rust build artifacts |
| `history` | `/commandhistory` | shell history |

`CLAUDE_CONFIG_DIR` is set to `/home/dev/.claude` so that Claude's
credentials file lands in that one directory rather than at `~/.claude.json`,
which lets a single volume cover all of its state. `CODEX_HOME` is set to
`/home/dev/.codex` for the same reason on the Codex side. omp needs no
equivalent — everything it keeps already lives under `~/.omp`.

Codex is the one whose binary shares a volume with its state: its installer
puts the payload under `$CODEX_HOME/packages` and makes `~/.local/bin/codex` a
symlink into it, so `codex-config` holds both. That used to need a workaround —
the image owned the payload at `/opt/codex` and `entrypoint.sh` linked the
volume at it, because otherwise the first image to fill the volume pinned codex
forever. With no agent in the image, the installer simply gets its own
directory back and the workaround is gone. `install-agents.sh` clears the stale
symlink if your `codex-config` volume predates the change.

opencode splits its state across two directories rather than one — settings in
`~/.config/opencode`, credentials and sessions in `~/.local/share/opencode` —
so it gets a volume for each, plus `opencode-bin` for its install prefix.
`opencode-data` nests inside the `agents` volume; Docker resolves the longest
matching mount path first, so it stays separate and survives a reinstall.

Not persisted: the rest of the container filesystem. Containers are started
with `--rm`, so anything installed ad-hoc inside a session is gone next time —
if you need it permanently, add it to the `Dockerfile`, or to
`install-agents.sh` if it is an agent.

### Why `target/` is a volume

The container is musl and your host is most likely glibc. Sharing one
`target/` directory would make the two toolchains invalidate each other's
artifacts on every switch, and worse, `just build-exe` links the object files
`omgc` produces with `cc` — mixing host and container output there would link
against the wrong libc. The host's own `target/` is left completely untouched.

## Environment variables passed through

These are forwarded from your shell into the container when they are set:
`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`,
`ANTHROPIC_MODEL`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`,
`OPENAI_API_KEY`, `GEMINI_API_KEY`, `OPENROUTER_API_KEY`, `AWS_REGION`,
`AWS_PROFILE`, `TERM`, `COLORTERM`.

You do not need an API key for normal use — the interactive login stored in
the `claude-config`/`codex-config`/`omp-config`/`opencode-data` volumes is
enough for each tool respectively. omp and opencode both speak to far more
providers than the keys listed
above; add the ones you use to `environment:` in `docker/compose.yaml`, or set
them in `~/.omp/.env` inside the container, which is on the volume.

Your git `user.name` and `user.email` are read from the host and applied
inside the container, so commits made there are attributed to you.

## Tips

- Claude runs unprivileged in an isolated container, which is the intended
  place for `./dev.sh claude --dangerously-skip-permissions` if you want it to
  work without approval prompts. Anything it does is still confined to the
  bind-mounted repo and the volumes. Codex's equivalent is
  `./dev.sh codex --dangerously-bypass-approvals-and-sandbox`. omp needs no
  flag at all: its default `tools.approvalMode` is already `yolo`. Going the
  other way, `./dev.sh omp --approval-mode always-ask` puts the prompts back.
- opencode has no equivalent flag — its permissions are config-driven. Set the
  `permission` block in `~/.config/opencode/opencode.json` to allow what you
  want run unattended; that file is on the `opencode-config` volume, so it is
  written once and survives every later run.
- omp discovers skills from `.claude/skills/` and `.codex/skills/` as well as
  its own `.omp/skills/`, so the skills this repo already carries show up in
  an `omp` session without being duplicated.
- Several sessions can run at once — each `./dev.sh` invocation is its own
  container, and they share the same volumes.
- Pushing over SSH from inside the container needs your key. The simplest
  route is to push from the host; alternatively add an agent-forwarding mount
  to `docker/compose.yaml`:
  ```yaml
  - ${SSH_AUTH_SOCK}:/ssh-agent
  ```
  with `SSH_AUTH_SOCK=/ssh-agent` in `environment:`.
- Building for the host's glibc is not what this image does; it produces musl
  binaries. That is fine for developing and testing `omgc`, but a release
  build for a glibc target should be done outside the container or with an
  added cross target.
