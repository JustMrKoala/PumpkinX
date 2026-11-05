#!/usr/bin/env bash
# Builds PumpkinX. Optional: --install copies the binary to /opt/pumpkinx and restarts PumpkinXTest.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROFILE="${PROFILE:-release}"
INSTALL=0
FEATURES=()
for arg in "$@"; do
    case "$arg" in
        --install) INSTALL=1 ;;
        --debug) PROFILE=dev ;;
        --no-patchbukkit) FEATURES=(--no-default-features) ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

if [ ${#FEATURES[@]} -eq 0 ]; then
    echo "==> PatchBukkit Java bridge (gradle)"
    (cd "$DIR/builtin/patchbukkit/java" && ./gradlew jar --no-daemon -q)
fi

echo "==> Keystone plugin (Wasm)"
(cd "$DIR/builtin/keystone" && cargo build --release --target wasm32-wasip2)

echo "==> PumpkinX ($PROFILE)"
cd "$DIR"
# PatchBukkit's generated code needs a deeper compiler stack.
RUST_MIN_STACK=33554432 cargo build -p pumpkin --profile "$PROFILE" "${FEATURES[@]}"

OUT="$DIR/target/$([ "$PROFILE" = dev ] && echo debug || echo "$PROFILE")/pumpkin"
echo "==> built $OUT"

if [ "$INSTALL" = 1 ]; then
    sudo install -D -m 0755 "$OUT" /opt/pumpkinx/pumpkinx
    sudo systemctl restart pumpkinxtest
    echo "==> installed to /opt/pumpkinx/pumpkinx and restarted pumpkinxtest"
fi
