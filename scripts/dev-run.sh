#!/usr/bin/env sh
# cargo `runner` for local development on macOS: re-sign the binary with a
# STABLE identity, then exec it as-is.
#
# Why: cargo debug builds carry an ad-hoc signature whose hash changes on every
# rebuild, so the macOS keychain treats each rebuilt binary as a brand-new app
# and re-prompts for authorization on every gogo command (live finding,
# 2026-10-10: five prompts after five rebuilds). Signing with a persistent
# identity — the Apple Development certificate from your keychain by default,
# overridable via GOGO_CODESIGN_IDENTITY — lets the keychain ACL match the
# certificate instead of the per-build hash, so a single "Always Allow"
# survives rebuilds.
#
# Best-effort by design: no identity found (Linux, CI, fresh machine) or a
# failing codesign never blocks the run — we just exec unsigned like before.
set -eu
BIN="$1"; shift

# Never re-sign test binaries (target/**/deps/*): cargo-nextest runs ONE process
# per test, re-invoking this runner each time, and codesigning a file that backs
# a still-running process makes the kernel SIGKILL that process (found live
# 2026-10-11: escalating SIGKILL test failures). Tests never touch the keychain
# (GOGO_HOME isolation), so they need no stable identity.
case "$BIN" in
    */deps/*) exec "$BIN" "$@" ;;
esac

if command -v codesign >/dev/null 2>&1; then
    IDENT="${GOGO_CODESIGN_IDENTITY:-}"
    if [ -z "$IDENT" ]; then
        IDENT=$(security find-identity -v -p codesigning 2>/dev/null \
            | sed -n 's/^[[:space:]]*[0-9]*) [0-9A-F]* "\(.*\)"$/\1/p' \
            | head -1)
    fi
    if [ -n "$IDENT" ]; then
        if ! codesign --force --sign "$IDENT" "$BIN" >/dev/null 2>&1; then
            echo "dev-run: codesign failed — running unsigned (expect keychain prompts)" >&2
        fi
    fi
fi

exec "$BIN" "$@"
