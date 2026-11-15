#!/usr/bin/env bash
# Type-check and lint the whole workspace for macOS from a Linux box:
# clippy -D warnings against aarch64-apple-darwin, then the tests of the
# portable crates natively. Nothing is linked, so C/C++ build scripts
# (OpenH264) get a stub compiler that only emits empty objects.
# Needs the GTK 4 + libadwaita dev packages and
# `rustup target add aarch64-apple-darwin`. Not a substitute for
# scripts/check.sh on a Mac: no macOS test runs here.
set -euo pipefail
cd "$(dirname "$0")/.."

stub=$(mktemp -d)
trap 'rm -rf "$stub"' EXIT
cat > "$stub/cc" <<'EOF'
#!/bin/sh
while [ $# -gt 0 ]; do
  case "$1" in -o) shift; : > "$1";; -Fo*) : > "${1#-Fo}";; esac; shift
done
EOF
chmod +x "$stub/cc"

echo "== fmt"
cargo fmt --all --check
echo "== clippy (aarch64-apple-darwin)"
PKG_CONFIG_ALLOW_CROSS=1 CC_aarch64_apple_darwin="$stub/cc" CXX_aarch64_apple_darwin="$stub/cc" \
    cargo clippy --workspace --all-targets --locked --target aarch64-apple-darwin -- -D warnings
echo "== test (portable crates)"
cargo test --locked -p gliff-proto -p gliff-transport -p gliff-sw
echo "CROSS-CHECK PASS"
