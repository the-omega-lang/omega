# Omega Ω

Rdbo's Omega programming language

*“I am Alpha and Omega, the First and the Last, the Beginning and the End.”* - **Revelations 22:13**

## Development environment

The whole toolchain (Rust, `cc`/`as`/`ld`, `just`, and Claude Code) is packaged
as a reproducible Alpine Linux container. With Docker installed, that is:

```sh
./dev.sh            # starts Claude Code in the container
./dev.sh shell      # or just a shell
./dev.sh run just build-exe
```

Nothing else needs to be installed on the host, and your Claude login, cargo
cache and build output persist across runs. See [`docker/README.md`](docker/README.md).

## License

This project is licensed under the `GNU AGPL-3.0`.

No other versions allowed.

Read the `LICENSE` file for more information.
