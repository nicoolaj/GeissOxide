# Makefile rules

All day-to-day operations go through `make`. Run `make help` to list the targets.

## Prerequisites

| Host | Needed |
|---|---|
| macOS (build + `darwin`) | Rust stable via `rustup` with targets `aarch64-apple-darwin` and `x86_64-apple-darwin`, Xcode Command Line Tools (`lipo`, `codesign`, libclang), `cargo-audit`, `cargo-deny`, `gh` (only for `make linux` from macOS) |
| Linux (build + `linux`) | Rust stable, `libasound2-dev` (ALSA headers), `cargo-audit`, `cargo-deny` |

Install the cargo tools once: `cargo install cargo-audit cargo-deny`.

## Variables

| Variable | Default | Purpose |
|---|---|---|
| `ARGS` | empty | Command-line options forwarded by `make run`, e.g. `make run ARGS="--engine milkdrop --device 1"` |
| `BUNDLE_ID` | `org.geissoxide.visualizer` | `CFBundleIdentifier` written into the `.app` |
| `DIST` | `dist` | Output directory for distributables |
| `VERSION` | read from `Cargo.toml` | Stamped into the `.app` `Info.plist` |

## Targets

| Target | What it runs | Produces |
|---|---|---|
| `make` / `make all` | `secu`, then `cargo build --release` | `target/release/geissoxide` for the current OS and CPU |
| `make run` | `all`, then `./target/release/geissoxide $(ARGS)` | runs the app |
| `make secu` (alias `make sécu`) | `cargo fmt --check`, `cargo clippy --all-targets --all-features -D warnings`, `cargo audit` (RustSec advisories), `cargo deny check` (advisories, licences, sources — policy in `deny.toml`), `cargo test` | fails on the first problem |
| `make darwin` | `secu`; release builds for `aarch64-apple-darwin` and `x86_64-apple-darwin`; `lipo -create`; assembles `Contents/MacOS/geissoxide`, `Contents/Info.plist` (from `packaging/Info.plist`, with `NSMicrophoneUsageDescription`), `Contents/Resources/GeissOxide.icns` (from `packaging/icon.iconset` via `iconutil`) and `Contents/Resources/presets` (if downloaded); ad-hoc `codesign` | `dist/GeissOxide.app` (universal binary) — macOS host only |
| `make linux` | On Linux: `secu`, `cargo build --release --target x86_64-unknown-linux-gnu`. On macOS: `secu`, then triggers the `release.yml` GitHub Actions workflow with `gh workflow run`, waits with `gh run watch`, downloads the artifact with `gh run download` | `dist/geissoxide-linux-amd64` |
| `make dist` | `darwin` then `linux` | both distributables |
| `make presets` | downloads the original MilkDrop preset pack (`projectM-visualizer/presets-milkdrop-original`, 552 `.milk` files) | `presets/` (git-ignored) |
| `make help` | prints the target list from the `##` comments | — |
| `make clean` | `cargo clean` | removes `target/` |
| `make mrproper` | `clean` + `rm -rf dist presets` | pristine checkout (the request said "mrpropoer"; the target is `mrproper`) |

## Linux builds from macOS (CI hand-off)

Pure-Rust cross-compiling to Linux would still need an ALSA sysroot, so Linux binaries are built
on a Linux runner. `make linux` on macOS requires:

1. the repository pushed to GitHub with `.github/workflows/release.yml`;
2. `gh auth login` done once;
3. the current branch pushed (the workflow runs on `git branch --show-current`).

The same workflow builds the macOS universal `.app` and, on a `v*` tag, attaches both files to a
GitHub Release.

## Troubleshooting

- `cargo: no such command: audit/deny` → `cargo install cargo-audit cargo-deny`.
- `make linux` on macOS says `gh` is missing → `brew install gh && gh auth login`.
- The `.app` shows no reaction to sound → macOS asked for microphone permission on first launch;
  check System Settings → Privacy & Security → Microphone. For system audio use BlackHole with a
  Multi-Output Device (see README).
- `codesign` fails → the ad-hoc signature only needs the Command Line Tools; run `xcode-select --install`.
