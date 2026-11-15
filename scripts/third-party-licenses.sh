#!/bin/sh
# Prints THIRD-PARTY-LICENSES.txt, what the released programs carry with
# their licenses: the crates (cargo-about, with about.toml and
# third-party-licenses.hbs; a crate under a license about.toml doesn't
# accept fails it), then the fonts and the logos.
set -eu
cd "$(dirname "$0")/.."

cargo about generate --locked scripts/third-party-licenses.hbs
for f in ui/fonts/LICENSE-*.txt assets/os/SOURCES.md; do
    echo "================================================================================"
    printf '%s:\n\n' "$f"
    cat "$f"
    echo
done
