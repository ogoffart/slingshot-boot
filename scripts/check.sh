#!/bin/sh
# What CI checks, on this machine: the unit tests, the lints of both
# targets, the setup script's tests and (with --ui) the UI tests. Run it
# before pushing.
#
# Without the prebuilt UEFI targets (`rustup target add x86_64-unknown-uefi
# aarch64-unknown-uefi`), core is built from source (needs rust-src).
set -e
cd "$(dirname "$0")/.."
UI=0
[ "${1:-}" = --ui ] && UI=1

BUILD_STD=""
if ! rustup target list --installed 2>/dev/null | grep -q x86_64-unknown-uefi; then
    BUILD_STD="-Zbuild-std=core,alloc,compiler_builtins -Zbuild-std-features=compiler-builtins-mem"
    export RUSTC_BOOTSTRAP=1
fi

echo "== unit tests"
SLINT_EMIT_DEBUG_INFO=1 cargo test --features system-testing --target-dir target/ui-tests
echo "== lints"
SLINT_EMIT_DEBUG_INFO=1 cargo clippy --tests --features system-testing --target-dir target/ui-tests -- -D warnings
# Without the tests too: code only the tests and the firmware use.
SLINT_EMIT_DEBUG_INFO=1 cargo clippy --features system-testing --target-dir target/ui-tests -- -D warnings
for target in x86_64-unknown-uefi aarch64-unknown-uefi; do
    # shellcheck disable=SC2086
    cargo clippy --release --target $target $BUILD_STD -- -D warnings
done
echo "== setup script"
scripts/test-setup.sh
if [ "$UI" = 1 ]; then
    echo "== UI tests"
    (cd ui-tests && uv run pytest -q)
fi
echo "All good."
