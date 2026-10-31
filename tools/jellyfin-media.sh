#!/usr/bin/env bash
# Makes the small library that tools/jellyfin-server.sh serves: three movies,
# a show with two seasons and an album, each file a few seconds long. Every
# item has an NFO file, so the server needs nothing from the internet to
# describe it. All but the episodes are marked <lockdata> as well, which keeps
# the server from looking them up online even if its fetchers were on; the
# episodes are left unlocked because a locked item gets no frame grabbed from
# the video for its picture.
#
# Usage: tools/jellyfin-media.sh DIR
#
# Writes DIR/Movies, DIR/Shows and DIR/Music, then DIR/.generated. Uses
# $FFMPEG, or ffmpeg from PATH; it needs libx264, libx265 and the lavfi test
# sources, which jellyfin-ffmpeg and most distribution builds have.
#
# What each file is for:
#   Night Signal (2021)   two audio tracks, an embedded Spanish subtitle, two
#                         external ones (.en.srt, .fr.forced.srt), chapters
#   Paper Harbor (2019)   MP4 with an embedded mov_text subtitle
#   The Long Field (2017) HEVC 10-bit with AC-3 5.1 and AAC stereo tracks
#   Lantern Street        seasons 1 (three episodes) and 2 (two); S01E01 has
#                         chapters named Intro, Part One and Credits
#   Mira Vale, Low Tide   four FLAC tracks with tags and embedded cover art;
#                         track 1 has timed .lrc lyrics, track 2 plain .txt
set -euo pipefail

if [ $# -ne 1 ]; then
  sed -n '2,24p' "$0"
  exit 2
fi
ROOT=$1
FFMPEG=${FFMPEG:-ffmpeg}

ff() { "$FFMPEG" -hide_banner -loglevel error -nostdin -y "$@"; }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# srt FILE START_SECONDS TEXT...: one two-second cue every three seconds.
srt() {
  local file=$1 t=$2 n=1
  shift 2
  : >"$file"
  for text in "$@"; do
    printf '%d\n00:00:%02d,000 --> 00:00:%02d,000\n%s\n\n' "$n" "$t" $((t + 2)) "$text" >>"$file"
    n=$((n + 1)) t=$((t + 3))
  done
}

# chapters FILE NAME:START_SECONDS... END_SECONDS, in ffmpeg's metadata format.
chapters() {
  local file=$1 end=${!#} i
  local -a marks=("${@:2:$#-2}")
  printf ';FFMETADATA1\n' >"$file"
  for ((i = 0; i < ${#marks[@]}; i++)); do
    local next=$end
    ((i + 1 < ${#marks[@]})) && next=${marks[i + 1]##*:}
    printf '[CHAPTER]\nTIMEBASE=1/1000\nSTART=%d\nEND=%d\ntitle=%s\n' \
      $((${marks[i]##*:} * 1000)) $((next * 1000)) "${marks[i]%:*}" >>"$file"
  done
}

# image FILE WIDTHxHEIGHT COLOUR COLOUR: a gradient, so that each poster has a
# blurhash of its own.
image() {
  ff -f lavfi -i "gradients=s=$2:c0=$3:c1=$4:x0=0:y0=0:x1=${2%x*}:y1=${2#*x}:d=1" \
    -frames:v 1 -q:v 4 "$1"
}

video_args=(-c:v libx264 -preset veryfast -crf 32 -pix_fmt yuv420p -g 48)

# --- Movies -----------------------------------------------------------------

m="$ROOT/Movies/Night Signal (2021)"
mkdir -p "$m"
srt "$TMP/es.srt" 1 "Hola" "Esto es una prueba" "Adiós"
chapters "$TMP/night.txt" Opening:0 Middle:10 Closing:20 30
ff -f lavfi -i "testsrc2=s=640x360:r=24:d=30" \
  -f lavfi -i "sine=f=440:r=48000:d=30" \
  -f lavfi -i "sine=f=660:r=48000:d=30" \
  -i "$TMP/es.srt" -i "$TMP/night.txt" \
  -map 0:v -map 1:a -map 2:a -map 3:s -map_chapters 4 \
  "${video_args[@]}" -c:a aac -b:a 64k -ac 2 -c:s srt \
  -metadata title="Night Signal" \
  -metadata:s:a:0 language=eng -metadata:s:a:0 title="Stereo" \
  -metadata:s:a:1 language=fra -metadata:s:a:1 title="Commentaire" \
  -metadata:s:s:0 language=spa \
  -disposition:v:0 default -disposition:a:0 default -disposition:a:1 0 \
  -disposition:s:0 0 "$m/Night Signal (2021).mkv"
srt "$m/Night Signal (2021).en.srt" 1 "Hello" "This is a test" "Goodbye"
srt "$m/Night Signal (2021).fr.forced.srt" 4 "Bonjour"
image "$m/poster.jpg" 400x600 0x14213d 0xfca311
image "$m/backdrop.jpg" 960x540 0x14213d 0xe5e5e5
cat >"$m/Night Signal (2021).nfo" <<'EOF'
<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<movie>
  <title>Night Signal</title>
  <originaltitle>Night Signal</originaltitle>
  <sorttitle>Night Signal</sorttitle>
  <year>2021</year>
  <premiered>2021-03-12</premiered>
  <plot>A radio operator on a remote coast hears a signal that repeats every thirty seconds.</plot>
  <tagline>Someone is still transmitting.</tagline>
  <runtime>1</runtime>
  <mpaa>PG-13</mpaa>
  <rating>7.1</rating>
  <genre>Mystery</genre>
  <genre>Drama</genre>
  <studio>Test Card Pictures</studio>
  <director>Ada Lindqvist</director>
  <actor>
    <name>Tomas Reyes</name>
    <role>Operator</role>
    <type>Actor</type>
    <sortorder>0</sortorder>
  </actor>
  <actor>
    <name>June Okafor</name>
    <role>Harbourmaster</role>
    <type>Actor</type>
    <sortorder>1</sortorder>
  </actor>
  <lockdata>true</lockdata>
</movie>
EOF

m="$ROOT/Movies/Paper Harbor (2019)"
mkdir -p "$m"
srt "$TMP/en.srt" 1 "The boats are made of paper" "They float anyway"
ff -f lavfi -i "smptebars=s=640x360:r=25:d=24" \
  -f lavfi -i "sine=f=330:r=48000:d=24" -i "$TMP/en.srt" \
  -map 0:v -map 1:a -map 2:s \
  "${video_args[@]}" -c:a aac -b:a 64k -ac 2 -c:s mov_text \
  -metadata:s:a:0 language=eng -metadata:s:s:0 language=eng \
  -disposition:s:0 0 -movflags +faststart "$m/Paper Harbor (2019).mp4"
image "$m/poster.jpg" 400x600 0x2a9d8f 0xe9c46a
cat >"$m/Paper Harbor (2019).nfo" <<'EOF'
<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<movie>
  <title>Paper Harbor</title>
  <year>2019</year>
  <premiered>2019-08-30</premiered>
  <plot>A town builds its fishing fleet out of paper and waits for a dry summer.</plot>
  <mpaa>PG</mpaa>
  <genre>Comedy</genre>
  <studio>Test Card Pictures</studio>
  <director>Ada Lindqvist</director>
  <lockdata>true</lockdata>
</movie>
EOF

m="$ROOT/Movies/The Long Field (2017)"
mkdir -p "$m"
ff -f lavfi -i "testsrc=s=640x360:r=25:d=20" \
  -f lavfi -i "sine=f=220:r=48000:d=20" \
  -f lavfi -i "sine=f=550:r=48000:d=20" \
  -map 0:v -map 1:a -map 2:a \
  -c:v libx265 -preset ultrafast -crf 34 -pix_fmt yuv420p10le -x265-params log-level=error \
  -c:a:0 ac3 -b:a:0 192k -ac:a:0 6 -c:a:1 aac -b:a:1 64k -ac:a:1 2 \
  -metadata:s:a:0 language=eng -metadata:s:a:1 language=deu \
  -disposition:v:0 default -disposition:a:0 default -disposition:a:1 0 \
  "$m/The Long Field (2017).mkv"
image "$m/poster.jpg" 400x600 0x606c38 0xfefae0
cat >"$m/The Long Field (2017).nfo" <<'EOF'
<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<movie>
  <title>The Long Field</title>
  <year>2017</year>
  <premiered>2017-05-19</premiered>
  <plot>Two farmers disagree about where one field ends and the next begins.</plot>
  <mpaa>G</mpaa>
  <genre>Drama</genre>
  <studio>Meadow Films</studio>
  <director>Pieter Haas</director>
  <lockdata>true</lockdata>
</movie>
EOF

# --- Shows ------------------------------------------------------------------

s="$ROOT/Shows/Lantern Street"
mkdir -p "$s"
image "$s/poster.jpg" 400x600 0x3d348b 0xf7b801
image "$s/backdrop.jpg" 960x540 0x3d348b 0xf18701
cat >"$s/tvshow.nfo" <<'EOF'
<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<tvshow>
  <title>Lantern Street</title>
  <year>2020</year>
  <premiered>2020-01-10</premiered>
  <plot>The shopkeepers of one narrow street keep their lights on through a long winter.</plot>
  <mpaa>TV-PG</mpaa>
  <genre>Comedy</genre>
  <studio>Test Card Television</studio>
  <status>Ended</status>
  <lockdata>true</lockdata>
</tvshow>
EOF
# Episodes are 20 seconds long: the server takes a picture for a first chapter
# at 15 seconds (or the end, if sooner), and one taken at the very end fails.
chapters "$TMP/pilot.txt" Intro:0 "Part One":4 Credits:16 20
episodes=(
  "1:1:Opening Night:2020-01-10"
  "1:2:Night Market:2020-01-17"
  "1:3:Open House:2020-01-24"
  "2:1:Back Again:2021-02-05"
  "2:2:Last Stop:2021-02-12"
)
for e in "${episodes[@]}"; do
  IFS=: read -r se ep title aired <<<"$e"
  d=$(printf '%s/Season %02d' "$s" "$se")
  b=$(printf 'Lantern Street S%02dE%02d' "$se" "$ep")
  mkdir -p "$d"
  chapter_args=()
  [ "$se$ep" = 11 ] && chapter_args=(-i "$TMP/pilot.txt" -map_chapters 2)
  ff -f lavfi -i "testsrc=s=640x360:r=25:d=20" \
    -f lavfi -i "sine=f=$((300 + se * 100 + ep * 20)):r=48000:d=20" \
    "${chapter_args[@]}" -map 0:v -map 1:a \
    -vf "hue=h=$((se * 90 + ep * 30))" \
    "${video_args[@]}" -c:a aac -b:a 48k -ac 2 -metadata:s:a:0 language=eng \
    -disposition:v:0 default -disposition:a:0 default "$d/$b.mkv"
  cat >"$d/$b.nfo" <<EOF
<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<episodedetails>
  <title>$title</title>
  <showtitle>Lantern Street</showtitle>
  <season>$se</season>
  <episode>$ep</episode>
  <aired>$aired</aired>
  <plot>Episode $ep of season $se of Lantern Street.</plot>
</episodedetails>
EOF
done

# --- Music ------------------------------------------------------------------

artist="$ROOT/Music/Mira Vale"
album="$artist/Low Tide (2022)"
mkdir -p "$album"
image "$TMP/cover.jpg" 500x500 0x023e8a 0x90e0ef
cat >"$artist/artist.nfo" <<'EOF'
<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<artist>
  <name>Mira Vale</name>
  <genre>Ambient</genre>
  <biography>Mira Vale records sine waves near the sea.</biography>
  <lockdata>true</lockdata>
</artist>
EOF
cat >"$album/album.nfo" <<'EOF'
<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<album>
  <title>Low Tide</title>
  <artist>Mira Vale</artist>
  <albumartist>Mira Vale</albumartist>
  <year>2022</year>
  <premiered>2022-06-03</premiered>
  <genre>Ambient</genre>
  <review>Four short tones, one for each hour of the tide going out.</review>
  <lockdata>true</lockdata>
</album>
EOF
tracks=("Low Tide:262" "Glass Water:294" "Night Ferry:330" "Harbor Lights:349")
for ((i = 0; i < ${#tracks[@]}; i++)); do
  title=${tracks[i]%:*} hz=${tracks[i]#*:} n=$((i + 1))
  ff -f lavfi -i "sine=f=$hz:r=44100:d=12" -i "$TMP/cover.jpg" \
    -map 0:a -map 1:v -c:a flac -ac 2 -c:v mjpeg -disposition:v attached_pic \
    -metadata:s:v title="Album cover" -metadata:s:v comment="Cover (front)" \
    -metadata title="$title" -metadata artist="Mira Vale" \
    -metadata album_artist="Mira Vale" -metadata album="Low Tide" \
    -metadata date=2022 -metadata genre=Ambient \
    -metadata track="$n/${#tracks[@]}" -metadata disc=1/1 \
    "$album/$(printf '%02d' "$n") - $title.flac"
done
cat >"$album/01 - Low Tide.lrc" <<'EOF'
[ti:Low Tide]
[ar:Mira Vale]
[al:Low Tide]
[length:00:12]
[00:00.50]The water leaves the harbour wall
[00:03.00]and takes the evening light
[00:06.00]the boats lie down on sand
[00:09.00]until the tide comes back tonight
EOF
cat >"$album/02 - Glass Water.txt" <<'EOF'
Glass water, still and clear
Every stone is showing here
Nothing moves and nothing sings
Only light on little things
EOF

# Jellyfin skips dot-files, so the marker is not scanned.
date -u +%FT%TZ >"$ROOT/.generated"
