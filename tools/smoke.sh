#!/bin/bash
# Runs Patina on a private Xvfb display, performs scripted input, and saves
# screenshots of its window.
#
# Usage: tools/smoke.sh OUTPUT_DIR NAME "PATINA ARGS" STEP...
#
# Each STEP is one of:
#   "shot LABEL"     save OUTPUT_DIR/NAME-LABEL.png, cropped to the window
#   "sleep SECONDS"  wait
#   anything else    passed to xdotool, e.g. "key ctrl+shift+p",
#                    "mousemove 640 400", "click 1"
#
# Needs Xvfb, xdotool and ImageMagick. Patina's settings and data go to
# OUTPUT_DIR/config and OUTPUT_DIR/data (or $PATINA_CONFIG_DIR), audio is
# discarded, and decoding is done in software because Xvfb has no GPU.
set -u
if [ $# -lt 3 ]; then
  sed -n '2,16p' "$0"
  exit 2
fi
OUT=$1 NAME=$2 ARGS=$3
shift 3
mkdir -p "$OUT/data" "${PATINA_CONFIG_DIR:-$OUT/config}"

TARGET=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
BIN=${PATINA_BIN:-$TARGET/debug/patina}
DISPLAY_NUM=${SMOKE_DISPLAY:-:97}

if ! xdpyinfo -display "$DISPLAY_NUM" >/dev/null 2>&1; then
  Xvfb "$DISPLAY_NUM" -screen 0 1600x1000x24 -nolisten tcp >/dev/null 2>&1 &
  sleep 1
fi

# shellcheck disable=SC2086 # ARGS is a word list on purpose.
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" \
  PATINA_CONFIG_DIR="${PATINA_CONFIG_DIR:-$OUT/config}" PATINA_DATA_DIR="$OUT/data" \
  RUST_LOG="${RUST_LOG:-info}" "$BIN" --mpv-ao=null --mpv-hwdec=no $ARGS >"$OUT/$NAME.log" 2>&1 &
PID=$!
sleep 3

# Without a window manager nothing gives the window keyboard focus.
WINDOW=$(DISPLAY=$DISPLAY_NUM xdotool search --pid $PID 2>/dev/null | head -1)
[ -n "$WINDOW" ] && DISPLAY=$DISPLAY_NUM xdotool windowfocus --sync "$WINDOW"

for step in "$@"; do
  case "$step" in
    shot\ *)
      file="$OUT/$NAME-${step#shot }.png"
      import -display "$DISPLAY_NUM" -window root "$file"
      if [ -n "$WINDOW" ]; then
        eval "$(DISPLAY=$DISPLAY_NUM xdotool getwindowgeometry --shell "$WINDOW")"
        magick "$file" -crop "${WIDTH}x${HEIGHT}+${X}+${Y}" +repage "$file"
      fi
      echo "$file"
      ;;
    sleep\ *) sleep "${step#sleep }" ;;
    # shellcheck disable=SC2086
    *) DISPLAY=$DISPLAY_NUM xdotool $step ;;
  esac
done

kill $PID 2>/dev/null
sleep 0.5
kill -9 $PID 2>/dev/null
wait $PID 2>/dev/null
echo "log: $OUT/$NAME.log"
