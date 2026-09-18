# Contributing

## Build

Requires Rust stable (`rustup`). Everything goes through `make` — see [MAKEFILE.md](MAKEFILE.md).

```bash
make            # checks + release build for this machine
make presets    # download the original MilkDrop preset pack (needed for the MilkDrop engine)
make run        # build and start (GeissOxide engine)
make run ARGS="--engine milkdrop --device 1"
make darwin     # macOS universal .app in dist/
make linux      # Linux amd64 executable in dist/ (native on Linux, via GitHub Actions from macOS)
```

For usage and audio setup, see [README.md](README.md).

## Before submitting changes

`make secu` (alias `make sécu`) must pass: `cargo fmt --check`, `cargo clippy --all-targets
--all-features -D warnings`, `cargo audit`, `cargo deny check`, `cargo test`. It also runs
automatically as part of `make`.

## Adding a language

The UI is translated via `locales/<lang>.yml`, with English (`locales/en.yml`) as the fallback
(see `src/i18n.rs`). Adding a language only requires a new `locales/<lang>.yml` with the same
keys as `locales/en.yml` — a test enforces that.
