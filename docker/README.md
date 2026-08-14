# Omega development container

A reproducible Alpine Linux environment for developing Omega, with Claude Code,
Codex CLI and omp (oh-my-pi) preinstalled and persistent across runs.

You need Docker (with the Conform plugin) and nothing else — no Rust, no
`just`, no Node on the host.

## Quick start

```sh
./dev.sh            # builds the image on first run, then starts Claude Code
./dev.sh codex      # same, but starts Codex CLI instead
./dev.sh omp        # same, but starts omp
```

The first run downloads the base image, the Rust toolchain and the three
agents (a few minutes). Every run after that starts in a second or two.

Log in to each tool once, inside the container; the login is stored in a
Docker volume and reused by every later run.

## Commands

| Command | What it does |
| --- | --- |
| `./dev.sh` | Start Claude Code in the container |
| `./dev.sh codex` | Start Codex CLI in the container |
| `./dev.sh omp` | Start omp (oh-my-pi) in the container |
| `./dev.sh shell` | Interactive bash shell in the container |
| `./dev.sh run <cmd...>` | Run a single command, e.g. `./dev.sh run cargo test` |
| `./dev.sh build` | Build the image if missing or out of date |
| `./dev.sh rebuild` | Rebuild from scratch (also how you update the agents) |
| `./dev.sh down` | Remove leftover containers, keep all volumes |
| `./dev.sh clean` | Remove containers **and volumes** (caches, history, agent logins) |
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
  invokes directly to assemble `shims` and link the object files `omgc`
  emits.
- **just** — the project's task runner.
- **Claude Code**, installed with the native installer
  (`https://claude.ai/install.sh`) into the unprivileged user's `~/.local`.
  On musl that installer lays down a self-contained executable whose only
  dynamic dependency is musl libc itself — no Node.js runtime in the image,
  and no glibc compatibility shims.
- **Codex CLI**, installed the same way with its own native installer
  (`https://chatgpt.com/codex/install.sh`) — also a self-contained musl
  binary into `~/.local/bin`, no Node.js involved.
- **omp (oh-my-pi)**, installed from `https://omp.sh/install` with `--binary`,
  which fetches the prebuilt `linux-musl` release into `~/.local/bin`. The
  flag matters: without it the installer prefers building from source through
  Bun, and would install Bun itself to do so. The musl build links
  `libstdc++`/`libgcc` dynamically, so both are in the apk list above.
- **ripgrep** from apk, which Claude Code uses as its search backend.
- **GNU userland** (`coreutils`, `findutils`, `grep`, `sed`, `diffutils`)
  instead of busybox's reduced applets, so shell commands behave the way
  tooling expects.
- A non-root `dev` user created with **your** uid/gid, so files the container
  writes into the repo are owned by you.

## Reproducibility

Every version is a pinned build argument in `docker/Dockerfile`, overridable
from the environment:

```sh
RUST_VERSION=1.95.0 ./dev.sh rebuild
ALPINE_VERSION=3.24 ./dev.sh rebuild
CLAUDE_CODE_VERSION=2.1.220 ./dev.sh rebuild
CODEX_VERSION=0.51.0 ./dev.sh rebuild
OMP_VERSION=v17.2.12 ./dev.sh rebuild
```

`CLAUDE_CODE_VERSION` takes whatever `claude install` takes: `stable` (the
default), `latest`, or an exact version. `CODEX_VERSION` takes whatever
Codex's own installer takes: `latest` (the default; there is no `stable`
channel) or an exact version. `OMP_VERSION` is `latest` (the default) or an
exact release tag — it is passed to the installer as `--ref`, so it carries
the leading `v`. Pin any of them to an exact version if you want two machines
to be byte-for-byte identical.

Claude Code's and Codex's in-place auto-updaters are disabled
(`DISABLE_AUTOUPDATER=1`, `CODEX_UPDATE_DISABLED=1`) so the image stays the
single source of truth — `./dev.sh rebuild` is how you move to a newer
release. Without that, a session would pull a large binary into a container
whose home directory is discarded on exit anyway. omp needs no such switch: it
only updates when you run `omp update` yourself, and doing that inside a
container is throwaway work for the same reason.

If a `rust-toolchain.toml` is ever added to the repo, rustup honours it inside
the container too, and it takes precedence over `RUST_VERSION`.

## What persists, and what does not

Persisted in named volumes (survive `./dev.sh down`, container restarts and
image rebuilds; removed only by `./dev.sh clean`):

| Volume | Mounted at | Contents |
| --- | --- | --- |
| `claude-config` | `/home/dev/.claude` | Claude Code login, settings, session history, todos |
| `codex-config` | `/home/dev/.codex` | Codex CLI login, settings, session state |
| `omp-config` | `/home/dev/.omp` | omp login, settings, session transcripts, blob store, memory |
| `cargo-registry` | `/usr/local/cargo/registry` | crates.io downloads |
| `cargo-git` | `/usr/local/cargo/git` | git dependency checkouts |
| `target` | `/workspace/target` | Rust build artifacts |
| `history` | `/commandhistory` | shell history |

`CLAUDE_CONFIG_DIR` is set to `/home/dev/.claude` so that Claude's
credentials file lands in that one directory rather than at `~/.claude.json`,
which lets a single volume cover all of its state. `CODEX_HOME` is set to
`/home/dev/.codex` for the same reason on the Codex side. omp needs no
equivalent — everything it keeps already lives under `~/.omp`.

Not persisted: the rest of the container filesystem. Containers are started
with `--rm`, so anything installed ad-hoc inside a session is gone next time —
if you need it permanently, add it to the `Dockerfile`.

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
the `claude-config`/`codex-config`/`omp-config` volumes is enough for each
tool respectively. omp speaks to far more providers than the keys listed
above; add the ones you use to `environment:` in `docker/conform.yaml`, or set
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
- omp discovers skills from `.claude/skills/` and `.codex/skills/` as well as
  its own `.omp/skills/`, so the skills this repo already carries show up in
  an `omp` session without being duplicated.
- Several sessions can run at once — each `./dev.sh` invocation is its own
  container, and they share the same volumes.
- Pushing over SSH from inside the container needs your key. The simplest
  route is to push from the host; alternatively add an agent-forwarding mount
  to `docker/conform.yaml`:
  ```yaml
  - ${SSH_AUTH_SOCK}:/ssh-agent
  ```
  with `SSH_AUTH_SOCK=/ssh-agent` in `environment:`.
- Building for the host's glibc is not what this image does; it produces musl
  binaries. That is fine for developing and testing `omgc`, but a release
  build for a glibc target should be done outside the container or with an
  added cross target.
