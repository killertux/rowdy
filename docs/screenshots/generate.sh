#!/usr/bin/env bash
# Regenerate the README screenshots with charmbracelet/vhs.
#
#   docs/screenshots/generate.sh            # every tape
#   docs/screenshots/generate.sh overview   # just tapes/overview.tape
#
# Builds rowdy, seeds the sample SQLite DB into a throwaway directory
# (with its own $HOME, so your ~/.rowdy is never read or written), saves
# a plaintext `shop` connection, then runs each tape. Requires `vhs`.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
command -v vhs >/dev/null || { echo "vhs not found — https://github.com/charmbracelet/vhs" >&2; exit 1; }

cargo build --release --manifest-path "$repo/Cargo.toml" --bin rowdy --example seed_sqlite
target="$(cargo metadata --format-version 1 --no-deps --manifest-path "$repo/Cargo.toml" \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"

export ROWDY_SHOTS_BIN="$target/release/rowdy"
export ROWDY_SHOTS_SEEDS="$here/seeds"
export ROWDY_SHOTS_WORK="$(mktemp -d)"
trap 'rm -rf "$ROWDY_SHOTS_WORK"' EXIT

mkdir -p "$ROWDY_SHOTS_WORK/home/.rowdy" "$ROWDY_SHOTS_WORK/db"
echo 'check_for_updates = false' > "$ROWDY_SHOTS_WORK/home/.rowdy/config.toml"
(
  cd "$ROWDY_SHOTS_WORK/db"
  "$target/release/examples/seed_sqlite" ./sample.db >/dev/null
  HOME="$ROWDY_SHOTS_WORK/home" "$ROWDY_SHOTS_BIN" connections add shop --url sqlite:./sample.db --password ""
)

cd "$here"
if [ $# -eq 0 ]; then
  tapes=(tapes/[!_]*.tape)
else
  tapes=("${@/#/tapes/}"); tapes=("${tapes[@]/%/.tape}")
fi
for tape in "${tapes[@]}"; do
  echo "→ $tape"
  vhs "$tape"
done
rm -f tapes/.out.gif
