#!/usr/bin/env bash
# Development loop from a Windows checkout: copies the working tree (as it
# is, uncommitted changes included) into the Linux filesystem of the distro
# and runs cargo there. Building straight on /mnt/<drive> works but is many
# times slower, because every file access crosses into Windows.
#
#   wsl -d archlinux -- bash /mnt/i/Projects/openclips/scripts/linux/dev.sh build
#   wsl -d archlinux -- bash .../dev.sh clippy --workspace --all-targets -- -D warnings
#   wsl -d archlinux -- bash .../dev.sh run -p openclips-app
#
# The copy lives in ~/build/openclips and keeps its own target directory, so
# only the first build is a full one.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
dest="${OPENCLIPS_BUILD_DIR:-$HOME/build/openclips}"
mkdir -p "$dest"

# Mirror the tree: drop what disappeared from the source, then copy over.
# The target directory and the git data stay out of it.
find "$dest" -mindepth 1 -maxdepth 1 ! -name target -exec rm -rf {} +
tar -C "$here" \
    --exclude=./target --exclude=./dist --exclude=./.git \
    --exclude='./*.md.bak' -cf - . | tar -C "$dest" -xf -

# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
# .cargo/config.toml carries defaults for the Windows GStreamer install,
# which only apply to variables that are not set. Set, they lose.
export PKG_CONFIG="${PKG_CONFIG:-pkg-config}"
export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-/usr/local/lib/pkgconfig}"
cd "$dest"
exec cargo "$@"
