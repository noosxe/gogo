#!/usr/bin/env sh
# Sign a built gogo binary with a stable local identity (see scripts/dev-run.sh
# for the rationale). Use when invoking target/debug/gogo directly — `cargo
# run`/`cargo test` sign automatically via the runner in .cargo/config.toml.
#
# Usage: scripts/dev-sign.sh [binary]    (default: target/debug/gogo)
set -eu
BIN="${1:-target/debug/gogo}"

if ! command -v codesign >/dev/null 2>&1; then
    echo "dev-sign: codesign not found (macOS only)" >&2
    exit 1
fi

IDENT="${GOGO_CODESIGN_IDENTITY:-}"
if [ -z "$IDENT" ]; then
    IDENT=$(security find-identity -v -p codesigning 2>/dev/null \
        | sed -n 's/^[[:space:]]*[0-9]*) [0-9A-F]* "\(.*\)"$/\1/p' \
        | head -1)
fi
if [ -z "$IDENT" ]; then
    echo "dev-sign: no codesigning identity found — create one with" >&2
    echo "  Keychain Access > Certificate Assistant > Create Certificate > Code Signing" >&2
    exit 1
fi

codesign --force --sign "$IDENT" "$BIN"
echo "dev-sign: signed $BIN with \"$IDENT\""
