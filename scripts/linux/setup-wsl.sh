#!/usr/bin/env bash
# Prepares a Linux machine (a WSL distro, a VM, a real install) for building
# OpenClips: the GStreamer development files, what Slint links against, the
# packaging tool of the distro family, and a Rust toolchain through rustup.
#
# Run it as root; the toolchain goes to the user named in BUILD_USER
# (default: the user that called sudo, or root itself):
#
#   sudo bash scripts/linux/setup-wsl.sh
#   wsl -d Ubuntu-22.04 -u root -- env BUILD_USER=me bash scripts/linux/setup-wsl.sh
#
# Safe to run again: everything it does is idempotent.
set -euo pipefail

if [ "$(id -u)" -ne 0 ]; then
    echo "run this as root (sudo, or wsl -u root)" >&2
    exit 1
fi

. /etc/os-release
family=""
for id in ${ID:-} ${ID_LIKE:-}; do
    case "$id" in
        arch | archlinux) family=arch ;;
        debian | ubuntu) family=debian ;;
        fedora | rhel) family=fedora ;;
        suse | opensuse*) family=suse ;;
    esac
    [ -n "$family" ] && break
done
if [ -z "$family" ]; then
    echo "unsupported distribution: ${PRETTY_NAME:-unknown}" >&2
    exit 1
fi
echo "== ${PRETTY_NAME:-$ID} (family: $family)"

case "$family" in
    arch)
        pacman -Syu --noconfirm --needed \
            base-devel git curl pkgconf clang \
            gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad \
            gst-plugins-ugly gst-libav gst-plugin-pipewire \
            fontconfig freetype2 libxkbcommon wayland libx11 libxcb libxcursor \
            libxrandr libxi mesa dbus \
            ttf-dejavu xdg-utils
        ;;
    debian)
        export DEBIAN_FRONTEND=noninteractive
        apt-get update
        apt-get install -y --no-install-recommends \
            build-essential git curl ca-certificates pkg-config clang \
            libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
            gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
            gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav \
            gstreamer1.0-pipewire gstreamer1.0-pulseaudio gstreamer1.0-x \
            libfontconfig1-dev libfreetype6-dev libxkbcommon-dev libxkbcommon-x11-dev \
            libwayland-dev libx11-dev libxcb1-dev libxcb-shape0-dev libxcb-xfixes0-dev \
            libxcursor-dev libxrandr-dev libxi-dev libgl1-mesa-dev libegl1-mesa-dev \
            libdbus-1-dev fonts-dejavu-core xdg-utils dpkg-dev
        ;;
    fedora)
        dnf install -y \
            gcc gcc-c++ make git curl pkgconf-pkg-config clang gawk findutils \
            gstreamer1-devel gstreamer1-plugins-base-devel \
            gstreamer1 gstreamer1-plugins-base gstreamer1-plugins-good \
            gstreamer1-plugins-bad-free gstreamer1-plugin-openh264 \
            gstreamer1-plugin-libav pipewire-gstreamer \
            fontconfig-devel freetype-devel libxkbcommon-devel libxkbcommon-x11-devel \
            wayland-devel libX11-devel libxcb-devel libXcursor-devel libXrandr-devel \
            libXi-devel mesa-libGL-devel mesa-libEGL-devel dbus-devel \
            dejavu-sans-fonts xdg-utils rpm-build
        ;;
    suse)
        zypper --non-interactive install \
            gcc gcc-c++ make git curl pkg-config clang \
            gstreamer-devel gstreamer-plugins-base-devel gstreamer-plugins-good \
            gstreamer-plugins-bad gstreamer-plugins-ugly gstreamer-plugins-libav \
            fontconfig-devel freetype2-devel libxkbcommon-devel wayland-devel \
            libX11-devel libxcb-devel Mesa-libGL-devel dbus-1-devel xdg-utils rpm-build
        ;;
esac

user="${BUILD_USER:-${SUDO_USER:-root}}"
home="$(getent passwd "$user" | cut -d: -f6)"
if [ -z "$home" ]; then
    echo "no such user: $user" >&2
    exit 1
fi
echo "== Rust toolchain for $user"
run_as() {
    if [ "$user" = root ]; then
        bash -lc "$1"
    else
        su - "$user" -c "$1"
    fi
}
if [ ! -x "$home/.cargo/bin/cargo" ]; then
    run_as "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component clippy,rustfmt"
fi
run_as "~/.cargo/bin/rustup update stable >/dev/null && ~/.cargo/bin/rustup component add clippy rustfmt >/dev/null 2>&1; ~/.cargo/bin/cargo --version"
echo "== done"
