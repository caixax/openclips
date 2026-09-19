#!/usr/bin/env bash
# Builds the release binary on this distro and packages it the way the
# distro family installs software: a .deb on Debian and Ubuntu, an .rpm on
# Fedora, a pacman package on Arch. With --tarball it also writes the generic
# archive that install.sh unpacks on everything else; build that one on the
# oldest distro at hand (its glibc is the floor for every other machine).
#
#   wsl -d Ubuntu-22.04 -- bash /mnt/i/Projects/openclips/scripts/linux/build.sh --tarball
#   wsl -d FedoraLinux-43 -- bash .../build.sh
#   wsl -d archlinux -- bash .../build.sh
#
# The working tree is copied into the Linux filesystem first (see dev.sh for
# why) and the packages land in dist/ of the checkout the script was run
# from. GStreamer is a dependency of the package, never bundled: hardware
# encoding only works through the GStreamer the distro built against its own
# drivers.
set -euo pipefail

tarball=0
[ "${1:-}" = "--tarball" ] && tarball=1

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="${OPENCLIPS_BUILD_DIR:-$HOME/build/openclips}"
out="$here/dist"
mkdir -p "$work" "$out"

find "$work" -mindepth 1 -maxdepth 1 ! -name target -exec rm -rf {} +
tar -C "$here" --exclude=./target --exclude=./dist --exclude=./.git -cf - . | tar -C "$work" -xf -

# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
export PKG_CONFIG="${PKG_CONFIG:-pkg-config}"
export PKG_CONFIG_PATH="${PKG_CONFIG_PATH:-/usr/local/lib/pkgconfig}"
cd "$work"

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
echo "== building OpenClips $version"
cargo build --release -p openclips-app
strip target/release/openclips

# The installed tree, shared by every format.
stage="$work/target/package/root"
rm -rf "$work/target/package"
install -Dm755 target/release/openclips "$stage/usr/bin/openclips"
install -Dm644 packaging/linux/openclips.desktop "$stage/usr/share/applications/openclips.desktop"
install -Dm644 crates/app/assets/icon.png "$stage/usr/share/icons/hicolor/256x256/apps/openclips.png"
install -Dm644 LICENSE "$stage/usr/share/licenses/openclips/LICENSE"
install -Dm644 README.md "$stage/usr/share/doc/openclips/README.md"

summary="Replay buffer and game clip recorder"
description="Keeps the last moments of the screen in memory and writes them to a clip
 when a key is pressed. Hardware encoding through NVENC or VA-API, separate
 audio tracks, a gallery and a trim editor. No account, no upload."

. /etc/os-release
family=""
for id in ${ID:-} ${ID_LIKE:-}; do
    case "$id" in
        arch | archlinux) family=arch ;;
        debian | ubuntu) family=debian ;;
        fedora | rhel) family=fedora ;;
    esac
    [ -n "$family" ] && break
done

case "$family" in
    debian)
        deb="$work/target/package/deb"
        cp -a "$stage" "$deb"
        # Debian keeps licenses under doc.
        mv "$deb/usr/share/licenses/openclips/LICENSE" "$deb/usr/share/doc/openclips/copyright"
        rm -rf "$deb/usr/share/licenses"
        mkdir -p "$deb/DEBIAN"
        size="$(du -sk "$deb/usr" | cut -f1)"
        cat > "$deb/DEBIAN/control" <<EOF
Package: openclips
Version: $version
Section: video
Priority: optional
Architecture: amd64
Maintainer: OpenClips contributors <noreply@github.com>
Homepage: https://github.com/caixax/openclips
Installed-Size: $size
Depends: libc6, libgstreamer1.0-0, libgstreamer-plugins-base1.0-0, gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-plugins-ugly, gstreamer1.0-libav, gstreamer1.0-pulseaudio, gstreamer1.0-x, libfontconfig1, libfreetype6, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libx11-6, libx11-xcb1, libxcb1, libxcursor1, libxrandr2, libxi6, libgl1, libegl1, libdbus-1-3, xdg-utils
Recommends: gstreamer1.0-pipewire, pipewire, xdg-desktop-portal, gstreamer1.0-vaapi, libnotify-bin
Description: $summary
 $description
EOF
        dpkg-deb --root-owner-group --build "$deb" "$out/openclips_${version}_amd64.deb"
        ;;
    fedora)
        top="$work/target/package/rpm"
        mkdir -p "$top"/{BUILD,RPMS,SPECS,SOURCES}
        cat > "$top/SPECS/openclips.spec" <<EOF
Name:           openclips
Version:        $version
Release:        1
Summary:        $summary
License:        MIT
URL:            https://github.com/caixax/openclips
Requires:       gstreamer1-plugins-base gstreamer1-plugins-good gstreamer1-plugins-bad-free gstreamer1-plugin-libav xdg-utils
Recommends:     pipewire-gstreamer gstreamer1-plugin-openh264 xdg-desktop-portal
# The binary is built outside of rpmbuild; there is nothing to strip again
# and no debug package to split off.
%global debug_package %{nil}
%global __strip /bin/true

%description
$description

%install
cp -a "$stage/." %{buildroot}/

%files
/usr/bin/openclips
/usr/share/applications/openclips.desktop
/usr/share/icons/hicolor/256x256/apps/openclips.png
%license /usr/share/licenses/openclips/LICENSE
%doc /usr/share/doc/openclips/README.md
EOF
        rpmbuild --define "_topdir $top" -bb "$top/SPECS/openclips.spec"
        cp "$top"/RPMS/x86_64/openclips-"$version"-1.x86_64.rpm "$out/openclips-${version}.x86_64.rpm"
        ;;
    arch)
        # Outside of the home directory: makepkg may run as another user
        # (see below), who cannot enter this one.
        pkg="$(mktemp -d /tmp/openclips-pkg.XXXXXX)"
        chmod 755 "$pkg"
        tar -C "$stage" -czf "$pkg/root.tar.gz" .
        cat > "$pkg/PKGBUILD" <<EOF
pkgname=openclips
pkgver=$version
pkgrel=1
pkgdesc="$summary"
arch=('x86_64')
url="https://github.com/caixax/openclips"
license=('MIT')
depends=('gstreamer' 'gst-plugins-base' 'gst-plugins-good' 'gst-plugins-bad' 'gst-plugins-ugly' 'gst-libav' 'fontconfig' 'freetype2' 'libxkbcommon' 'libxkbcommon-x11' 'wayland' 'libx11' 'libxcb' 'libxcursor' 'libxrandr' 'libxi' 'libglvnd' 'dbus' 'xdg-utils')
optdepends=('gst-plugin-pipewire: capture on Wayland'
            'xdg-desktop-portal: capture on Wayland'
            'gst-plugin-va: hardware encoding on Intel and AMD'
            'libnotify: clip saved notifications')
options=('!strip' '!debug')
source=('root.tar.gz')
sha256sums=('SKIP')
noextract=('root.tar.gz')
package() {
    tar -C "\$pkgdir" -xzf "\$srcdir/root.tar.gz"
}
EOF
        # makepkg refuses to run as root, which is what a WSL Arch is.
        if [ "$(id -u)" -eq 0 ]; then
            id builder >/dev/null 2>&1 || useradd -m builder
            chown -R builder "$pkg"
            su builder -c "cd '$pkg' && PKGDEST='$pkg' makepkg -f --nodeps"
        else
            (cd "$pkg" && PKGDEST="$pkg" makepkg -f --nodeps)
        fi
        cp "$pkg"/openclips-"$version"-1-x86_64.pkg.tar.zst "$out/openclips-${version}-x86_64.pkg.tar.zst"
        ;;
    *)
        echo "no package format for ${PRETTY_NAME:-this distro}; use --tarball" >&2
        [ "$tarball" -eq 1 ] || exit 1
        ;;
esac

if [ "$tarball" -eq 1 ]; then
    name="openclips-${version}-linux-x86_64"
    tree="$work/target/package/$name"
    mkdir -p "$tree"
    cp -a "$stage/usr/." "$tree/"
    tar -C "$work/target/package" -czf "$out/$name.tar.gz" "$name"
fi

echo "== packages in $out"
ls -l "$out" | grep -i "openclips" || true
