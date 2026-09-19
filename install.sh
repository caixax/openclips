#!/usr/bin/env bash
# OpenClips installer for Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/caixax/openclips/main/install.sh | bash
#
# Finds out which distribution, desktop, session and GPU this machine has,
# downloads the matching package of the latest release, checks it against the
# release's SHA256SUMS.txt and installs it with the system's package manager,
# which pulls in GStreamer and the rest. Then it adds what depends on the
# machine: the screen sharing portal of the desktop, the PipeWire plugin on
# Wayland, and the hardware encoder plugin for the GPU.
#
# Options (after `bash -s --` when piping):
#   --version X.Y.Z   install that release instead of the latest
#   --yes             do not ask before installing packages
#   --uninstall       remove OpenClips (settings and clips stay)
#   --dry-run         show what would be done and do nothing
set -euo pipefail

REPO="caixax/openclips"
version=""
assume_yes=0
uninstall=0
dry_run=0
while [ $# -gt 0 ]; do
    case "$1" in
        --version) version="${2:-}"; shift ;;
        --yes | -y) assume_yes=1 ;;
        --uninstall) uninstall=1 ;;
        --dry-run) dry_run=1 ;;
        -h | --help) sed -n '2,18p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

if [ -t 1 ]; then
    bold=$'\033[1m'; dim=$'\033[2m'; red=$'\033[31m'; green=$'\033[32m'; yellow=$'\033[33m'; reset=$'\033[0m'
else
    bold=""; dim=""; red=""; green=""; yellow=""; reset=""
fi
step() { printf '%s==>%s %s\n' "$bold" "$reset" "$*"; }
note() { printf '    %s%s%s\n' "$dim" "$*" "$reset"; }
warn() { printf '%swarning:%s %s\n' "$yellow" "$reset" "$*" >&2; }
die() { printf '%serror:%s %s\n' "$red" "$reset" "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# ------------------------------------------------------------------ machine

[ "$(uname -s)" = "Linux" ] || die "this installer is for Linux; Windows has its own setup on the releases page"
[ "$(uname -m)" = "x86_64" ] || die "only x86_64 builds exist for now (this is $(uname -m))"
have curl || die "curl is needed to download the release"

# shellcheck disable=SC1091
. /etc/os-release 2>/dev/null || die "cannot read /etc/os-release"
family=""
for id in ${ID:-} ${ID_LIKE:-}; do
    case "$id" in
        arch | archlinux | manjaro | endeavouros | cachyos) family=arch ;;
        debian | ubuntu | linuxmint | pop) family=debian ;;
        fedora | rhel | centos | nobara) family=fedora ;;
        suse | opensuse*) family=suse ;;
    esac
    [ -n "$family" ] && break
done
[ -n "$family" ] || family=other

# The session and desktop decide how the screen is captured; see the app's
# own detection, which makes the same call at run time.
session="${XDG_SESSION_TYPE:-}"
if [ -z "$session" ]; then
    if [ -n "${WAYLAND_DISPLAY:-}" ]; then session=wayland; elif [ -n "${DISPLAY:-}" ]; then session=x11; else session=unknown; fi
fi
desktop_raw="${XDG_CURRENT_DESKTOP:-${DESKTOP_SESSION:-}}"
desktop="$(printf '%s' "$desktop_raw" | tr '[:upper:]' '[:lower:]')"
case "$desktop" in
    *kde* | *plasma*) desktop=kde ;;
    *gnome* | *unity* | *budgie* | *pantheon*) desktop=gnome ;;
    *hyprland*) desktop=hyprland ;;
    *sway* | *river* | *wayfire* | *labwc* | *niri*) desktop=wlroots ;;
    *cosmic*) desktop=cosmic ;;
    *xfce* | *cinnamon* | *mate* | *lxqt* | *lxde*) desktop=gtk ;;
    "") desktop=unknown ;;
    *) desktop=other ;;
esac

# GPU vendors from the DRM devices (0x10de NVIDIA, 0x8086 Intel, 0x1002 AMD).
gpus=""
for vendor_file in /sys/class/drm/card*/device/vendor; do
    [ -r "$vendor_file" ] || continue
    case "$(cat "$vendor_file")" in
        0x10de) gpus="$gpus nvidia" ;;
        0x8086) gpus="$gpus intel" ;;
        0x1002) gpus="$gpus amd" ;;
    esac
done
gpus="$(printf '%s\n' $gpus | sort -u | tr '\n' ' ' | sed 's/ $//')"

step "This machine"
note "distribution: ${PRETTY_NAME:-$ID} (family: $family)"
note "desktop:      ${desktop_raw:-unknown} -> $desktop, session: $session"
note "GPU:          ${gpus:-none detected}"

sudo_cmd=""
if [ "$(id -u)" -ne 0 ]; then
    have sudo || die "sudo is needed to install packages (or run this as root)"
    sudo_cmd="sudo"
fi
run() {
    if [ "$dry_run" -eq 1 ]; then
        note "would run: $*"
    else
        "$@"
    fi
}
confirm() {
    [ "$assume_yes" -eq 1 ] && return 0
    [ "$dry_run" -eq 1 ] && return 0
    # Piped from curl, stdin is the script; the terminal is the way to ask.
    if [ -r /dev/tty ]; then
        printf '%s [Y/n] ' "$1" > /dev/tty
        read -r answer < /dev/tty || answer=""
    else
        return 0
    fi
    case "$answer" in n | N | no | NO) return 1 ;; *) return 0 ;; esac
}

# ---------------------------------------------------------------- uninstall

if [ "$uninstall" -eq 1 ]; then
    step "Removing OpenClips"
    case "$family" in
        debian) run $sudo_cmd apt-get remove -y openclips ;;
        fedora) run $sudo_cmd dnf remove -y openclips ;;
        suse) run $sudo_cmd zypper --non-interactive remove openclips ;;
        arch) run $sudo_cmd pacman -R --noconfirm openclips ;;
        *) : ;;
    esac
    prefix="${XDG_DATA_HOME:-$HOME/.local/share}"
    run rm -f "$HOME/.local/bin/openclips" "$prefix/applications/openclips.desktop" \
        "$prefix/icons/hicolor/256x256/apps/openclips.png" \
        "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/openclips.desktop"
    note "settings (~/.config/openclips) and your clips were left alone"
    exit 0
fi

# ----------------------------------------------------------------- download

step "Finding the release"
if [ -z "$version" ]; then
    tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
    [ -n "$tag" ] || die "could not read the latest release from GitHub"
    version="${tag#v}"
else
    tag="v${version#v}"
    version="${version#v}"
fi
case "$family" in
    debian) asset="openclips_${version}_amd64.deb" ;;
    fedora | suse) asset="openclips-${version}.x86_64.rpm" ;;
    arch) asset="openclips-${version}-x86_64.pkg.tar.zst" ;;
    *) asset="openclips-${version}-linux-x86_64.tar.gz" ;;
esac
base="https://github.com/$REPO/releases/download/$tag"
note "OpenClips $version: $asset"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fetch() {
    curl -fL --progress-bar -o "$tmp/$1" "$base/$1"
}
if [ "$dry_run" -eq 0 ]; then
    if ! fetch "$asset"; then
        # A release without a package for this family still has the archive.
        warn "$asset is not part of this release, using the generic archive"
        asset="openclips-${version}-linux-x86_64.tar.gz"
        family_install=other
        fetch "$asset" || die "release $tag has no Linux build"
    fi
    curl -fsSL -o "$tmp/SHA256SUMS.txt" "$base/SHA256SUMS.txt" || die "release $tag has no SHA256SUMS.txt"
    expected="$(awk -v f="$asset" '$2 == f { print $1 }' "$tmp/SHA256SUMS.txt")"
    [ -n "$expected" ] || die "SHA256SUMS.txt does not list $asset"
    actual="$(sha256sum "$tmp/$asset" | cut -d' ' -f1)"
    [ "$expected" = "$actual" ] || die "checksum mismatch for $asset, nothing was installed"
    note "checksum verified"
fi
family_install="${family_install:-$family}"

# ------------------------------------------------------------------ install

# What the machine needs on top of the package's own dependencies.
extras=""
add() { extras="$extras $*"; }
portal_for() {
    case "$desktop" in
        kde) echo "xdg-desktop-portal-kde" ;;
        gnome) echo "xdg-desktop-portal-gnome" ;;
        hyprland) echo "xdg-desktop-portal-hyprland" ;;
        wlroots) echo "xdg-desktop-portal-wlr" ;;
        cosmic) echo "xdg-desktop-portal-cosmic" ;;
        *) echo "xdg-desktop-portal-gtk" ;;
    esac
}
case "$family_install" in
    debian)
        [ "$session" = "x11" ] || add xdg-desktop-portal "$(portal_for)" gstreamer1.0-pipewire pipewire
        case " $gpus " in *" intel "*) add gstreamer1.0-vaapi intel-media-va-driver ;; esac
        case " $gpus " in *" amd "*) add gstreamer1.0-vaapi mesa-va-drivers ;; esac
        add libnotify-bin
        ;;
    fedora)
        [ "$session" = "x11" ] || add xdg-desktop-portal "$(portal_for)" pipewire-gstreamer
        add gstreamer1-plugin-openh264
        case " $gpus " in *" intel "*) add libva-intel-media-driver ;; esac
        ;;
    suse)
        [ "$session" = "x11" ] || add xdg-desktop-portal "$(portal_for)" gstreamer-plugin-pipewire
        ;;
    arch)
        [ "$session" = "x11" ] || add xdg-desktop-portal "$(portal_for)" gst-plugin-pipewire
        case " $gpus " in *" intel "*) add gst-plugin-va intel-media-driver ;; esac
        case " $gpus " in *" amd "*) add gst-plugin-va libva-mesa-driver ;; esac
        add libnotify
        ;;
esac

step "Installing"
note "package: $asset"
[ -n "$extras" ] && note "for this machine:$extras"
confirm "Install OpenClips $version and the packages above?" || die "cancelled, nothing was installed"

# Extras are best effort: a package that a distro names differently must
# not stop the install of the app itself.
install_extras() {
    [ -n "$extras" ] || return 0
    for package in $extras; do
        if ! run "$@" "$package" >/dev/null 2>&1; then
            warn "could not install $package (not in your repositories?)"
        fi
    done
}
case "$family_install" in
    debian)
        run $sudo_cmd apt-get update -qq || true
        run $sudo_cmd apt-get install -y "$tmp/$asset"
        install_extras $sudo_cmd apt-get install -y
        ;;
    fedora)
        run $sudo_cmd dnf install -y "$tmp/$asset"
        install_extras $sudo_cmd dnf install -y
        ;;
    suse)
        run $sudo_cmd zypper --non-interactive install --allow-unsigned-rpm "$tmp/$asset"
        install_extras $sudo_cmd zypper --non-interactive install
        ;;
    arch)
        run $sudo_cmd pacman -U --noconfirm "$tmp/$asset"
        install_extras $sudo_cmd pacman -S --noconfirm --needed
        ;;
    *)
        # No known package manager: the archive goes under ~/.local and the
        # user is told what it needs.
        prefix="$HOME/.local"
        run mkdir -p "$prefix"
        run tar -C "$prefix" --strip-components=1 -xzf "$tmp/$asset"
        note "installed to $prefix/bin/openclips"
        case ":$PATH:" in *":$prefix/bin:"*) ;; *) warn "$prefix/bin is not on your PATH" ;; esac
        warn "install GStreamer 1.20 or newer with the base, good, bad, ugly and libav plugin sets from your distribution"
        ;;
esac

# -------------------------------------------------------------------- check

if [ "$dry_run" -eq 0 ] && have gst-inspect-1.0; then
    step "Checking the encoders"
    found_video=""
    for enc in nvh264enc vah264enc vah264lpenc vaapih264enc qsvh264enc x264enc openh264enc; do
        if gst-inspect-1.0 "$enc" >/dev/null 2>&1; then found_video="$found_video $enc"; fi
    done
    found_audio=""
    for enc in fdkaacenc avenc_aac voaacenc faac; do
        if gst-inspect-1.0 "$enc" >/dev/null 2>&1; then found_audio="$found_audio $enc"; fi
    done
    [ -n "$found_video" ] && note "H.264:$found_video" || warn "no H.264 encoder was found"
    [ -n "$found_audio" ] && note "AAC:  $found_audio" || warn "no AAC encoder was found"
    if [ -z "$found_video" ] && [ "$family" = "fedora" ]; then
        warn "Fedora ships H.264 separately: sudo dnf install gstreamer1-plugin-openh264, or enable RPM Fusion for x264 and VA-API H.264"
    fi
    case " $gpus " in
        *" nvidia "*)
            case "$found_video" in *nvh264enc*) ;; *) warn "NVIDIA GPU without nvh264enc: it needs the proprietary driver and the GStreamer nvcodec plugin (part of plugins-bad)" ;; esac
            ;;
    esac
    if [ "$session" != "x11" ] && ! gst-inspect-1.0 pipewiresrc >/dev/null 2>&1; then
        warn "pipewiresrc is missing: screen capture on Wayland needs the GStreamer PipeWire plugin"
    fi
fi

if [ "$dry_run" -eq 1 ]; then
    printf '\nDry run: nothing was downloaded or installed.\n'
    exit 0
fi
printf '\n%sOpenClips %s is installed.%s Start it from your applications menu or run: openclips\n' "$green" "$version" "$reset"
if [ "$session" != "x11" ]; then
    note "Wayland: bind keys in your desktop's shortcut settings to"
    note "  openclips --save-clip   openclips --toggle-recording   openclips --toggle-buffer"
fi
