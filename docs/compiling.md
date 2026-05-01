# Compiling from source

A pre-built wheel is the supported install path: it lays down both the
CLI binary and the Python module, no toolchain required. While the
`udoc` name on PyPI is being secured, install via the project's PEP
503 index:

```bash
pip install udoc --index-url https://newelh.github.io/udoc/simple/
```

This page is for the cases where you want to build the wheel
yourself.

## Prerequisites

- **Rust** — stable toolchain. udoc tracks the latest stable; the
  workspace MSRV is `1.88`. Install via [rustup](https://rustup.rs/).
- **Python** — 3.10 or newer. The Python wheel uses the abi3-py310
  stable ABI, so one wheel covers 3.10 / 3.11 / 3.12 / 3.13.
- **A C linker** — whatever `cc` or `clang` your platform expects.
  No further C dependencies.

udoc has zero native runtime dependencies. No `libpoppler`, no `libfreetype`, no Office install, and no system fonts. Everything required for extraction lives inside the binary.

## Clone

```bash
git clone https://github.com/newelh/udoc
cd udoc
```

## Build the Python wheel

The wheel ships two things: the PyO3 native extension (so
`import udoc` works) **and** the `udoc` CLI binary (so the `udoc`
console script works). Maturin handles the extension; the CLI binary
is staged into `python/udoc/_bin/` before the wheel is packed so
maturin's `python-source` step picks it up.

For local iteration:

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install maturin

# Stage the CLI binary into the python source tree.
cargo build --release -p udoc --bin udoc
mkdir -p python/udoc/_bin
cp target/release/udoc python/udoc/_bin/udoc

maturin develop --release
python -c "import udoc; print(udoc.__version__)"
udoc --version
```

`maturin develop` compiles the Rust extension into a `.so` (or
`.pyd` on Windows) and installs it into the active virtualenv along
with the bundled CLI binary. Re-run after changes; the build is
incremental.

For a distributable wheel:

```bash
cargo build --release -p udoc --bin udoc
mkdir -p python/udoc/_bin
cp target/release/udoc python/udoc/_bin/udoc

maturin build --release
ls target/wheels/
```

The output lands in `target/wheels/udoc-*.whl`. Install it anywhere
with `pip install <path-to-wheel>`. Both `udoc --version` and
`python -c "import udoc"` should work afterwards.

`scripts/build-wheels.sh` automates the stage-then-build sequence and
also runs cibuildwheel on top, in case you want to verify the
manylinux flow on a Linux box.

## Build for multiple platforms

The release pipeline runs `cibuildwheel` to produce wheels for Linux
x86_64 + aarch64, macOS x86_64 + arm64, and Windows x86_64. To
reproduce locally:

```bash
pip install cibuildwheel
./scripts/build-wheels.sh                 # current platform
./scripts/build-wheels.sh --all-platforms # whatever your host can target
```

cibuildwheel uses Docker (manylinux) on Linux, the host toolchain on
macOS and Windows. The matrix lives in the `[tool.cibuildwheel]`
section of `pyproject.toml`.

## Build just the CLI binary

If you do not want the Python module — say you are scripting against
the CLI from a non-Python environment — `cargo build` produces the
binary on its own:

```bash
cargo build --release -p udoc --bin udoc
./target/release/udoc --version
```

This is the binary that ships inside the wheel. The CLI surface,
features, and exit codes are identical.

## Running the test suite

```bash
cargo test --workspace                    # all Rust tests
cargo test --workspace --doc              # doctests on public APIs
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Python tests run via pytest after a `maturin develop`:

```bash
pip install pytest
pytest python/tests/
```

The pre-commit hook in `scripts/install-hooks.sh` runs format, clippy,
and tests on every commit; install once and you will not push a
broken commit by accident.

## Cross-compilation

The Rust side cross-compiles cleanly with the standard
`cargo build --target=...` once you have the platform's linker on
your `$PATH`. For macOS-from-Linux, [`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild)
is the path of least resistance; for Windows, the Rust `x86_64-pc-windows-gnu`
target works under MinGW.

For the Python wheel side, `maturin build --target=...` does the
right thing once the Rust target is installed. cibuildwheel hides
all of this behind a config matrix when you can run the release
pipeline.

## Building the docs

The hosted manual is plain markdown under `docs/` served by GitHub
Pages from the `main` branch. To preview locally, the project ships
a small Python server in the repo:

```bash
python -m http.server --directory docs/
```

…or use any markdown previewer of your choice. The CI workflow in
`.github/workflows/docs.yml` deploys `docs/` on every push to main.

For the per-crate API reference (rustdoc):

```bash
cargo doc --workspace --no-deps --open
```

## crates.io

udoc is not on crates.io for the alpha period. Distribution is via
the PEP 503 index above; PyPI publishing follows once the project
name is secured. Per-crate publishing to crates.io — `udoc`,
`udoc-core`, `udoc-pdf`, and the per-format backends as independent
dependencies — lands at beta, once the public API has stabilised
across at least one external integration.

If you need the Rust API in the meantime, depend on the workspace by
git path:

```toml
[dependencies]
udoc = { git = "https://github.com/newelh/udoc", tag = "v0.1.0-alpha.1" }
```

This is supported but the API is alpha; expect to bump frequently.
