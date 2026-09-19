#!/usr/bin/env bash
# Runs a command inside a throwaway Wayland session with a working
# ScreenCast portal, so the portal capture path can be exercised on a
# machine without a desktop (WSL, CI): headless sway, PipeWire, WirePlumber,
# xdg-desktop-portal and its wlroots backend, all on a private D-Bus session.
#
#   bash scripts/linux/portal-test.sh cargo run -p openclips-capture --example clip_check -- 5 /tmp/clip.mp4
#
# Needs (Arch names): sway xdg-desktop-portal xdg-desktop-portal-wlr pipewire
# wireplumber foot. The wlroots portal is told to share the only output
# without asking, which stands in for the user's click in the dialog.
set -euo pipefail

if [ "${1:-}" != "--inside" ]; then
    export XDG_RUNTIME_DIR="$(mktemp -d /tmp/openclips-portal.XXXXXX)"
    chmod 700 "$XDG_RUNTIME_DIR"
    trap 'rm -rf "$XDG_RUNTIME_DIR"' EXIT
    exec dbus-run-session -- bash "$0" --inside "$@"
fi
shift

log="$XDG_RUNTIME_DIR/log"
mkdir -p "$log" "$XDG_RUNTIME_DIR/config/xdg-desktop-portal-wlr" "$XDG_RUNTIME_DIR/config/xdg-desktop-portal"
export XDG_CONFIG_HOME="$XDG_RUNTIME_DIR/config"
export XDG_STATE_HOME="$XDG_RUNTIME_DIR/state"
cat > "$XDG_CONFIG_HOME/xdg-desktop-portal-wlr/config" <<'EOF'
[screencast]
chooser_type=none
output_name=HEADLESS-1
max_fps=60
EOF
cat > "$XDG_CONFIG_HOME/xdg-desktop-portal/portals.conf" <<'EOF'
[preferred]
default=wlr
EOF

unset WAYLAND_DISPLAY DISPLAY
pids=()
cleanup() { kill "${pids[@]}" 2>/dev/null || true; }
trap cleanup EXIT

WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
    sway -c /dev/null >"$log/sway.log" 2>&1 &
pids+=($!)
for _ in $(seq 50); do
    [ -S "$XDG_RUNTIME_DIR/wayland-1" ] && break
    sleep 0.1
done
export WAYLAND_DISPLAY=wayland-1 XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=sway
dbus-update-activation-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE XDG_CONFIG_HOME 2>/dev/null || true

pipewire >"$log/pipewire.log" 2>&1 &
pids+=($!)
sleep 0.5
wireplumber >"$log/wireplumber.log" 2>&1 &
pids+=($!)
sleep 1
/usr/lib/xdg-desktop-portal-wlr -l DEBUG >"$log/xdpw.log" 2>&1 &
pids+=($!)
sleep 0.5
/usr/lib/xdg-desktop-portal >"$log/xdp.log" 2>&1 &
pids+=($!)
sleep 1.5

# Something that moves, so the compositor has frames to send.
if command -v foot >/dev/null; then
    foot sh -c 'while :; do date +%s.%N; sleep 0.05; done' >/dev/null 2>&1 &
    pids+=($!)
    sleep 0.5
fi

status=0
"$@" || status=$?
if [ "$status" -ne 0 ] || [ -n "${PORTAL_TEST_LOGS:-}" ]; then
    for f in "$log"/*.log; do
        echo "---- $f"
        tail -n 25 "$f"
    done
fi
exit "$status"
