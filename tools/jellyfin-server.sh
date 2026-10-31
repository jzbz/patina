#!/usr/bin/env bash
# Runs a throwaway Jellyfin server on 127.0.0.1 for Patina's Jellyfin tests
# and for trying the client by hand. Everything it downloads, makes and keeps
# lives in one folder, DIR, so deleting DIR undoes it.
#
# Usage:
#   tools/jellyfin-server.sh setup DIR          download the server and ffmpeg, make the library
#   tools/jellyfin-server.sh start DIR [PORT]   start the server, on port 18096 unless given
#   tools/jellyfin-server.sh stop DIR           stop it
#   tools/jellyfin-server.sh fixtures DIR OUT   save the server's answers as JSON test fixtures
#
# start prints the variables the tests read, so that
#   eval "$(tools/jellyfin-server.sh start DIR)"
# points them at the server. The first start sets the server up through its
# API: the administrator "patina", a second user "viewer" (password
# "viewer-test", listed on the sign-in screen, which patina is not), the
# libraries Movies, Shows and Music with trickplay images, a collection, a
# playlist, and a scan. patina has watched Lantern Street S01E01 and half of
# Paper Harbor, so Next Up and Continue Watching have something in them.
#
# The server listens on 127.0.0.1 only, which still lets in the other users
# of this computer and any web page open in a browser here. So patina's
# password is made at random at the first start and kept in
# DIR/admin-password, which only this user can read, and every start has the
# server allow cross-origin requests from its own address only, so that no
# other page can send it a token or read its answers. viewer, whose password
# is no secret, can see the generated library and nothing else.
#
# Jellyfin 12.4 makes no media segments by itself (its segment providers are
# all plugins, and /MediaSegments lists only what an installed provider made),
# so /MediaSegments answers with an empty list here.
#
# Needs bash, curl, jq, tar, xz and setsid, on x86_64 Linux.
set -euo pipefail

# The official portable build carries its own .NET; jellyfin-ffmpeg is the
# ffmpeg the server is built and tested against. repo.jellyfin.org publishes
# no checksum, so the first is pinned to a download's digest; the second is
# the digest GitHub gives for the release asset.
SERVER_URL=https://repo.jellyfin.org/files/server/linux/stable/v12.4/amd64/jellyfin_12.4-amd64.tar.xz
SERVER_SHA256=29831bfda5699018d1d3058085c6b837fb31da34b5eab3c98259aa12da80375f
FFMPEG_URL=https://github.com/jellyfin/jellyfin-ffmpeg/releases/download/v8.1.3-1/jellyfin-ffmpeg_8.1.3-1_portable_linux64-gpl.tar.xz
FFMPEG_SHA256=b86dc023c64a5a9a410e6e3a938a970460214fae78f31c1553c9f88f1c289b6a

ADMIN_USER='patina'
# The administrator's password before this script made one at random, which
# a server set up by an earlier version still has until its next start.
PUBLISHED_ADMIN_PASSWORD='patina-test'
VIEWER_USER='viewer'
VIEWER_PASSWORD='viewer-test'
SERVER_NAME="Patina Test Server"
DEFAULT_PORT=18096

TOOL_AUTH='MediaBrowser Client="Patina test server", Device="jellyfin-server.sh", DeviceId="patina-test-server", Version="1"'
FIXTURE_AUTH='MediaBrowser Client="Patina", Device="Patina on Linux", DeviceId="fixture-device", Version="0.1.0"'

say() { printf 'jellyfin-server: %s\n' "$*" >&2; }
die() {
  say "$*"
  exit 1
}
usage() {
  sed -n '2,/^[^#]/{/^#/p}' "$0" >&2
  exit 2
}

# use_dir DIR [create]: sets DIR to the absolute form of DIR. The server
# stores the library's paths as given, so a relative one would break when the
# script is run from elsewhere.
use_dir() {
  [ -n "$1" ] || usage
  if [ -n "${2:-}" ]; then
    mkdir -p "$1"
  fi
  [ -d "$1" ] || die "no such folder: $1"
  DIR=$(cd "$1" && pwd -P)
}

# --- Downloads --------------------------------------------------------------

verified() { printf '%s  %s\n' "$1" "$2" | sha256sum --check --status -; }

# fetch URL SHA256: downloads into DIR/downloads unless a good copy is there.
fetch() {
  local url=$1 sha=$2 file
  file=$DIR/downloads/${url##*/}
  if [ -f "$file" ] && verified "$sha" "$file"; then
    return
  fi
  say "downloading ${url##*/}"
  curl -fsSL --retry 3 --proto '=https' --proto-redir '=https' -A Patina-dev \
    -o "$file.part" "$url"
  if ! verified "$sha" "$file.part"; then
    rm -f "$file.part"
    die "${url##*/} does not match its pinned SHA-256"
  fi
  mv "$file.part" "$file"
}

setup() {
  [ "$(uname -sm)" = "Linux x86_64" ] || die "this runs the linux-amd64 build, on x86_64 Linux only"
  ! server_pid >/dev/null || die "the server is running; stop it first"
  mkdir -p "$DIR/downloads"
  fetch "$SERVER_URL" "$SERVER_SHA256"
  fetch "$FFMPEG_URL" "$FFMPEG_SHA256"

  rm -rf "$DIR/jellyfin" "$DIR/jellyfin-ffmpeg"
  tar -C "$DIR" -xJf "$DIR/downloads/${SERVER_URL##*/}"
  mkdir "$DIR/jellyfin-ffmpeg"
  tar -C "$DIR/jellyfin-ffmpeg" -xJf "$DIR/downloads/${FFMPEG_URL##*/}"

  # The library is made once: the server keeps its items by path, and a
  # second set of files would only make it rescan.
  if [ ! -f "$DIR/media/.generated" ]; then
    say "making the test library"
    rm -rf "${DIR:?}/media"
    FFMPEG=$DIR/jellyfin-ffmpeg/ffmpeg "$(dirname "$0")/jellyfin-media.sh" "$DIR/media"
  fi
  say "set up in $DIR"
}

# --- The server process -----------------------------------------------------

# alive PID: true while the process runs. A process that has exited but not
# been reaped still answers kill -0, so its state is checked as well.
alive() {
  local stat
  stat=$(cat "/proc/$1/stat" 2>/dev/null) || return 1
  stat=${stat##*) }
  [ "${stat%% *}" != Z ]
}

# Prints the running server's pid. The command line is checked too, so that a
# stale pid file never makes this script signal some unrelated process.
server_pid() {
  local pid
  pid=$(cat "$DIR/server.pid" 2>/dev/null) || return 1
  [ -n "$pid" ] && alive "$pid" || return 1
  tr '\0' '\n' <"/proc/$pid/cmdline" 2>/dev/null | grep -qxF -- "$DIR/data" || return 1
  printf '%s\n' "$pid"
}

port_in_use() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

# valid_port PORT: true for a TCP port number, leading zeros allowed.
valid_port() { [[ $1 =~ ^[0-9]{1,5}$ ]] && ((10#$1 > 0 && 10#$1 <= 65535)); }

# read_port: sets PORT and URL from DIR/server.port. What the file holds ends
# up in the variables start prints for eval, so it must be a port and nothing
# else.
read_port() {
  PORT=$(cat "$DIR/server.port" 2>/dev/null) || die "no port in $DIR/server.port"
  valid_port "$PORT" || die "$DIR/server.port does not hold a port"
  PORT=$((10#$PORT))
  URL=http://127.0.0.1:$PORT
}

# The server reads its port and addresses from network.xml. It is written
# before every start, so the server always listens on 127.0.0.1 alone whatever
# it saved last time. LocalNetworkAddresses is what keeps HTTP on loopback
# (an address that matches no interface would make it listen everywhere, so
# it is the literal 127.0.0.1); client discovery on UDP 7359 ignores that
# setting and listens on every interface, so it is turned off, as are UPnP
# (obsolete in 12.4, but still read) and access from outside the LAN.
write_network_xml() {
  cat >"$DIR/config/network.xml" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<NetworkConfiguration xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema">
  <BaseUrl />
  <EnableHttps>false</EnableHttps>
  <RequireHttps>false</RequireHttps>
  <InternalHttpPort>$PORT</InternalHttpPort>
  <PublicHttpPort>$PORT</PublicHttpPort>
  <AutoDiscovery>false</AutoDiscovery>
  <EnableUPnP>false</EnableUPnP>
  <EnableIPv4>true</EnableIPv4>
  <EnableIPv6>false</EnableIPv6>
  <EnableRemoteAccess>false</EnableRemoteAccess>
  <LocalNetworkAddresses>
    <string>127.0.0.1</string>
  </LocalNetworkAddresses>
  <EnablePublishedServerUriByRequest>false</EnablePublishedServerUriByRequest>
</NetworkConfiguration>
EOF
}

# Starts the server in a session of its own, so that Ctrl-C in the shell that
# started it doesn't reach it. All its state goes under DIR: the four folders
# it is given, its temporary folder ($TMPDIR/jellyfin), fontconfig's cache
# and anything else that follows HOME. Its outgoing HTTP goes to a proxy on a
# closed port, so that nothing it does reaches the internet: the library
# settings already turn the online metadata sources off, but plugin updates,
# people and studio images don't follow them.
launch() {
  mkdir -p "$DIR"/{config,data,cache,log,tmp,home/.cache,home/.config,home/.local/share}
  local proxy=http://127.0.0.1:9
  (
    cd "$DIR"
    exec env HOME="$DIR/home" TMPDIR="$DIR/tmp" \
      XDG_CACHE_HOME="$DIR/home/.cache" XDG_CONFIG_HOME="$DIR/home/.config" \
      XDG_DATA_HOME="$DIR/home/.local/share" \
      http_proxy=$proxy https_proxy=$proxy all_proxy=$proxy \
      HTTP_PROXY=$proxy HTTPS_PROXY=$proxy ALL_PROXY=$proxy \
      no_proxy=127.0.0.1,localhost NO_PROXY=127.0.0.1,localhost \
      setsid "$DIR/jellyfin/jellyfin" \
      --datadir "$DIR/data" --configdir "$DIR/config" \
      --cachedir "$DIR/cache" --logdir "$DIR/log" \
      --ffmpeg "$DIR/jellyfin-ffmpeg/ffmpeg" --nonetchange
  ) >"$DIR/log/console.log" 2>&1 </dev/null &
  echo $! >"$DIR/server.pid"
}

# Waits for /health to say Healthy, for three minutes by the clock (a server
# that takes connections and never answers makes each try last its curl's two
# seconds). While the server starts, a stand-in on the same port answers
# /health with "Degraded" and /System/Info/Public with StartupWizardCompleted
# false whatever the truth, so neither of those is enough; "Unhealthy" means
# startup failed, and the server would then wait ten minutes before exiting.
wait_until_ready() {
  local pid=$1 deadline health
  deadline=$(($(date +%s) + 180))
  while :; do
    if ! alive "$pid"; then
      tail -n 20 "$DIR/log/console.log" >&2
      die "the server exited while starting; its logs are in $DIR/log"
    fi
    health=$(curl -s --max-time 2 "$URL/health" || true)
    case $health in
      Healthy) break ;;
      Unhealthy)
        stop
        die "the server could not start; its logs are in $DIR/log"
        ;;
    esac
    if [ "$(date +%s)" -ge "$deadline" ]; then
      stop
      die "the server was not ready after three minutes"
    fi
    sleep 0.5
  done
  api GET /System/Info/Public >/dev/null
}

start() {
  [ -x "$DIR/jellyfin/jellyfin" ] && [ -f "$DIR/media/.generated" ] ||
    die "nothing to start in $DIR; run setup first"
  [ "$(id -u)" != 0 ] || die "run this as an ordinary user, not root"
  local pid started
  if pid=$(server_pid); then
    read_port
    say "already running (pid $pid) at $URL"
    secure >&2
    print_env
    return
  fi
  port_in_use "$PORT" && die "port $PORT is in use; pass another one"

  started=$(date +%s)
  mkdir -p "$DIR/config"
  write_network_xml
  echo "$PORT" >"$DIR/server.port"
  launch
  pid=$(cat "$DIR/server.pid")
  wait_until_ready "$pid"
  say "server up at $URL (pid $pid) after $(($(date +%s) - started)) s"
  # Output goes to stderr, keeping stdout for the variables.
  if [ ! -f "$DIR/setup.done" ]; then
    first_start >&2
    touch "$DIR/setup.done"
    say "first-start setup done after $(($(date +%s) - started)) s"
  fi
  secure >&2
  print_env
}

# The variables the tests read, quoted for eval. PATINA_TEST_JELLYFIN_LIBRARY
# tells them the library is this script's, whose contents they may count on.
print_env() {
  local password
  password=$(admin_password)
  printf 'export PATINA_TEST_JELLYFIN_URL=%q\n' "$URL"
  printf 'export PATINA_TEST_JELLYFIN_USER=%q\n' "$ADMIN_USER"
  printf 'export PATINA_TEST_JELLYFIN_PASSWORD=%q\n' "$password"
  printf 'export PATINA_TEST_JELLYFIN_LIBRARY=%q\n' generated
}

stop() {
  local pid i
  if ! pid=$(server_pid); then
    rm -f "$DIR/server.pid"
    say "not running"
    return 0
  fi
  # SIGTERM is the server's ordinary shutdown: it finishes its database
  # writes before exiting.
  kill -TERM "$pid"
  for ((i = 0; i < 60; i++)); do
    alive "$pid" || break
    sleep 0.5
  done
  if alive "$pid"; then
    say "still running after 30 s; killing it"
    kill -KILL "$pid"
    # Only a process stuck in the kernel outlives that for long.
    for ((i = 0; i < 100; i++)); do
      alive "$pid" || break
      sleep 0.1
    done
    if alive "$pid"; then
      die "still running 10 s after it was killed (pid $pid)"
    fi
  fi
  rm -f "$DIR/server.pid"
  say "stopped"
}

# --- The API ----------------------------------------------------------------

# api METHOD PATH [BODY]: prints the answer and fails on an HTTP error, or
# when none has come in a minute. BODY is JSON, or "-" for JSON on stdin.
# AUTH and TOKEN make the Authorization header.
AUTH=$TOOL_AUTH
TOKEN=
api() {
  local auth=$AUTH
  [ -z "$TOKEN" ] || auth+=", Token=\"$TOKEN\""
  local -a args=(-sS --fail-with-body --max-time 60 -A Patina-dev -X "$1" -H "Authorization: $auth")
  if [ $# -gt 2 ]; then
    args+=(-H 'Content-Type: application/json')
    if [ "$3" = - ]; then args+=(--data-binary @-); else args+=(--data-binary "$3"); fi
  elif [ "$1" = POST ]; then
    args+=(-H 'Content-Length: 0')
  fi
  curl "${args[@]}" "$URL$2"
}

# sign_in USER PASSWORD: sets TOKEN.
sign_in() {
  TOKEN=
  TOKEN=$(jq -nc --arg u "$1" --arg p "$2" '{Username: $u, Pw: $p}' |
    api POST /Users/AuthenticateByName - | jq -er .AccessToken)
}

# A random password, 24 characters that need no quoting.
new_password() { head -c 18 /dev/urandom | base64 | tr '+/' '-_'; }

# Prints patina's password, from DIR/admin-password.
admin_password() {
  [ -s "$DIR/admin-password" ] ||
    die "no $DIR/admin-password; start the server with this script to make one"
  cat "$DIR/admin-password"
}

# Makes DIR/admin-password, readable by this user only, unless it is there.
make_admin_password() {
  [ -s "$DIR/admin-password" ] && return
  (umask 077 && new_password >"$DIR/admin-password.new")
  mv "$DIR/admin-password.new" "$DIR/admin-password"
}

# Signs in as patina. A server set up by an earlier version of this script
# still has the password that version published; it gets a random one here.
sign_in_admin() {
  if [ -s "$DIR/admin-password" ]; then
    sign_in "$ADMIN_USER" "$(admin_password)"
    return
  fi
  say "replacing the administrator's published password with a random one"
  sign_in "$ADMIN_USER" "$PUBLISHED_ADMIN_PASSWORD"
  make_admin_password
  # The token this request carries outlives the change; patina's others don't.
  jq -nc --arg old "$PUBLISHED_ADMIN_PASSWORD" --arg new "$(admin_password)" \
    '{CurrentPw: $old, NewPw: $new}' | api POST /Users/Password -
}

# Run at every start: signs in as patina, and has the server allow
# cross-origin requests from its own address only. Its default, "*", lets
# any web page open in a browser on this computer sign in and read the
# answers; with an origin named, the server sends the CORS headers to that
# origin alone, and a browser then refuses other pages the requests that
# carry a token or a JSON body. A server set up before this was done allows
# every origin until this has run, for a moment after its start.
secure() {
  sign_in_admin
  api GET /System/Configuration | jq -c --arg origin "$URL" '.CorsHosts = [$origin]' |
    api POST /System/Configuration -
}

# Library settings for a scan that stays offline and leaves the media folder
# alone: no online metadata or image sources for any item type, only the
# ffmpeg frame and cover grabbers; nothing saved beside the media; chapter
# and trickplay images made during the scan for video.
add_library() { # NAME COLLECTION_TYPE FOLDER IS_VIDEO ITEM_TYPE...
  local name=$1 kind=$2 folder=$3 video=$4 types
  shift 4
  types=$(printf '%s\n' "$@" | jq -R . | jq -sc .)
  jq -nc --arg path "$DIR/media/$folder" --argjson video "$video" --argjson types "$types" '{
    LibraryOptions: {
      Enabled: true,
      EnableRealtimeMonitor: false,
      EnablePhotos: false,
      EnableChapterImageExtraction: $video,
      ExtractChapterImagesDuringLibraryScan: $video,
      EnableTrickplayImageExtraction: $video,
      ExtractTrickplayImagesDuringLibraryScan: $video,
      SaveTrickplayWithMedia: false,
      EnableLUFSScan: false,
      SaveLocalMetadata: false,
      MetadataSavers: [],
      SaveSubtitlesWithMedia: false,
      SaveLyricsWithMedia: false,
      AutomaticRefreshIntervalDays: 0,
      PreferredMetadataLanguage: "en",
      MetadataCountryCode: "US",
      PathInfos: [{Path: $path}],
      TypeOptions: [$types[] | {
        Type: .,
        MetadataFetchers: [],
        MetadataFetcherOrder: [],
        ImageFetchers: (
          if . == "Movie" or . == "Episode" then ["Embedded Image Extractor", "Screen Grabber"]
          elif . == "Audio" then ["Image Extractor"]
          else [] end),
        ImageFetcherOrder: []
      }]
    }
  }' | api POST "/Library/VirtualFolders?name=$name&collectionType=$kind&refreshLibrary=false" -
}

# Scans every library and waits for the scan task to finish, for ten minutes
# by the clock. Trickplay is set to block the scan, so its images are done
# too.
scan() {
  local since result
  since=$(date -u +%s)
  api POST /Library/Refresh
  while [ "$(date -u +%s)" -lt $((since + 600)) ]; do
    sleep 0.5
    result=$(api GET '/ScheduledTasks?isHidden=false' | jq -r --argjson since "$since" '
      .[] | select(.Key == "RefreshLibrary") | .LastExecutionResult as $r
      | if .State == "Idle" and $r != null
          and ($r.EndTimeUtc | sub("\\.[0-9]+"; "") | sub("Z$"; "") + "Z" | fromdateiso8601) >= $since
        then $r.Status else empty end')
    case $result in
      "") ;;
      Completed) return ;;
      *) die "the library scan ended with status $result" ;;
    esac
  done
  die "the library scan did not finish in ten minutes"
}

# item_id QUERY: the id of the first item /Items finds for QUERY.
item_id() { api GET "/Items?recursive=true&$1" | jq -er '.Items[0].Id'; }

first_start() {
  TOKEN=
  # The wizard. GET /Startup/User is what creates the first user (named after
  # the account running the server); POST renames it and sets its password.
  if ! api GET /System/Info/Public | jq -e .StartupWizardCompleted >/dev/null; then
    say "completing the setup wizard"
    make_admin_password
    api GET /Startup/User >/dev/null
    jq -nc --arg name "$SERVER_NAME" '{ServerName: $name, UICulture: "en-US",
      MetadataCountryCode: "US", PreferredMetadataLanguage: "en"}' |
      api POST /Startup/Configuration -
    jq -nc --arg n "$ADMIN_USER" --arg p "$(admin_password)" '{Name: $n, Password: $p}' |
      api POST /Startup/User -
    api POST /Startup/RemoteAccess '{"EnableRemoteAccess": false}'
    api POST /Startup/Complete
  fi
  sign_in_admin

  # Server settings. Quick Connect is on by default; it is set here because
  # the tests need it. The plugin catalogue is emptied, since its update task
  # would otherwise try the internet at every start. The test files are a few
  # seconds long, so the server is told to keep resume points for anything
  # over a second (by default it ignores files under five minutes) and to
  # make a trickplay image every two seconds (by default ten, which most of
  # these files are shorter than). Studios and people take their settings from
  # here rather than from a library, so their online sources are turned off
  # here.
  api GET /System/Configuration | jq -c --arg name "$SERVER_NAME" '
    .ServerName = $name
    | .QuickConnectAvailable = true
    | .PluginRepositories = []
    | .MinResumeDurationSeconds = 1
    | .TrickplayOptions.Interval = 2000
    | .TrickplayOptions.ScanBehavior = "Blocking"
    | .MetadataOptions |= map(select(.ItemType != "Studio" and .ItemType != "Person")) + [
        {ItemType: "Studio", DisabledMetadataFetchers: [], DisabledImageFetchers: ["Artwork Repository"]},
        {ItemType: "Person", DisabledMetadataFetchers: ["TheMovieDb"], DisabledImageFetchers: ["TheMovieDb"]}]' |
    api POST /System/Configuration -

  local folders
  folders=$(api GET /Library/VirtualFolders | jq -r '.[].Name')
  grep -qx Movies <<<"$folders" || add_library Movies movies Movies true Movie
  grep -qx Shows <<<"$folders" || add_library Shows tvshows Shows true Series Season Episode
  grep -qx Music <<<"$folders" || add_library Music music Music false MusicArtist MusicAlbum Audio
  say "scanning the library"
  scan

  local counts
  counts=$(api GET /Items/Counts)
  say "items: $(jq -c '{MovieCount, SeriesCount, EpisodeCount, AlbumCount, SongCount, ArtistCount}' <<<"$counts")"
  jq -e '.MovieCount == 3 and .SeriesCount == 1 and .EpisodeCount == 5
    and .AlbumCount == 1 and .SongCount == 4' <<<"$counts" >/dev/null ||
    die "the scan did not find the expected items"
  say "trickplay images for $(api GET '/Items?recursive=true&includeItemTypes=Movie,Episode&fields=Trickplay' |
    jq '[.Items[] | select((.Trickplay // {}) | length > 0)] | length') of 8 videos"

  local users viewer
  users=$(api GET /Users)
  if ! jq -e --arg n "$VIEWER_USER" 'any(.[]; .Name == $n)' <<<"$users" >/dev/null; then
    viewer=$(jq -nc --arg n "$VIEWER_USER" --arg p "$VIEWER_PASSWORD" '{Name: $n, Password: $p}' |
      api POST /Users/New - | jq -er .Id)
    # New users are hidden from the sign-in screen by default.
    api GET "/Users/$viewer" | jq -c '.Policy | .IsHidden = false' |
      api POST "/Users/$viewer/Policy" -
  fi

  local admin_id collection movie1 movie2 tracks
  admin_id=$(api GET /Users/Me | jq -er .Id)
  if ! api GET '/Items?recursive=true&includeItemTypes=BoxSet' | jq -e '.TotalRecordCount > 0' >/dev/null; then
    movie1=$(item_id 'includeItemTypes=Movie&searchTerm=Night%20Signal')
    movie2=$(item_id 'includeItemTypes=Movie&searchTerm=Paper%20Harbor')
    # Locked, or the server would look the collection up online. The movies
    # are added afterwards: 12.4 drops the ones given when it is created.
    collection=$(api POST '/Collections?name=Harbour%20Lights%20Collection&isLocked=true' | jq -er .Id)
    api POST "/Collections/$collection/Items?ids=$movie1,$movie2"
  fi
  if ! api GET '/Items?recursive=true&includeItemTypes=Playlist' | jq -e '.TotalRecordCount > 0' >/dev/null; then
    tracks=$(api GET '/Items?recursive=true&includeItemTypes=Audio&sortBy=IndexNumber' |
      jq -c '[.Items[] | .Id] | [.[2], .[0], .[3]]')
    jq -nc --arg user "$admin_id" --argjson ids "$tracks" \
      '{Name: "Evening Mix", Ids: $ids, UserId: $user, MediaType: "Audio"}' |
      api POST /Playlists - >/dev/null
  fi

  # Something watched, and something half watched.
  local episode movie
  episode=$(item_id 'includeItemTypes=Episode&parentIndexNumber=1&indexNumber=1')
  api POST "/UserPlayedItems/$episode" >/dev/null
  movie=$(item_id 'includeItemTypes=Movie&searchTerm=Paper%20Harbor')
  local report
  report=$(jq -nc --arg id "$movie" '{ItemId: $id, MediaSourceId: $id,
    PlaySessionId: "test-server-setup", PlayMethod: "DirectPlay", CanSeek: true}')
  api POST /Sessions/Playing "$(jq -c '.PositionTicks = 0' <<<"$report")"
  api POST /Sessions/Playing/Progress "$(jq -c '.PositionTicks = 120000000 | .IsPaused = false' <<<"$report")"
  api POST /Sessions/Playing/Stopped "$(jq -c '.PositionTicks = 120000000' <<<"$report")"
}

# --- Fixtures ---------------------------------------------------------------

# The device profile Patina sends with PlaybackInfo at Auto quality, as
# DeviceProfile::patina makes it (docs/jellyfin.md): direct play of anything,
# HLS when the server must transcode. A test in src/jellyfin/tests.rs checks
# that the two are the same.
DEVICE_PROFILE='{
  "Name": "Patina",
  "MaxStreamingBitrate": 1000000000,
  "MaxStaticBitrate": 1000000000,
  "MusicStreamingTranscodingBitrate": 320000,
  "DirectPlayProfiles": [{"Type": "Video"}, {"Type": "Audio"}],
  "TranscodingProfiles": [
    {"Type": "Video", "Container": "ts", "Protocol": "hls", "Context": "Streaming",
     "VideoCodec": "h264,hevc", "AudioCodec": "aac,ac3,eac3,mp3", "MaxAudioChannels": "8"},
    {"Type": "Audio", "Container": "mp3", "Protocol": "http", "Context": "Streaming",
     "AudioCodec": "mp3", "MaxAudioChannels": "2"}
  ],
  "CodecProfiles": [],
  "SubtitleProfiles": [
    {"Format": "subrip", "Method": "External"}, {"Format": "srt", "Method": "External"},
    {"Format": "ass", "Method": "External"}, {"Format": "ssa", "Method": "External"},
    {"Format": "webvtt", "Method": "External"}, {"Format": "vtt", "Method": "External"},
    {"Format": "pgssub", "Method": "External"},
    {"Format": "subrip", "Method": "Embed"}, {"Format": "srt", "Method": "Embed"},
    {"Format": "ass", "Method": "Embed"}, {"Format": "ssa", "Method": "Embed"},
    {"Format": "webvtt", "Method": "Embed"}, {"Format": "mov_text", "Method": "Embed"},
    {"Format": "pgssub", "Method": "Embed"}, {"Format": "dvdsub", "Method": "Embed"},
    {"Format": "dvbsub", "Method": "Embed"}
  ]
}'

# save NAME: writes the JSON on stdin to OUT/NAME.json, pretty, with DIR,
# the token and the Quick Connect secret replaced, so that no fixture holds a
# path from this machine or a credential.
SECRET=
save() {
  jq --arg dir "$DIR" --arg token "$TOKEN" --arg secret "$SECRET" '
    def replace($from; $to): if $from == "" then . else split($from) | join($to) end;
    walk(if type == "string"
      then replace($dir; "/srv/jellyfin") | replace($token; "TOKEN") | replace($secret; "SECRET")
      else . end)' >"$OUT/$1.json"
}

fixtures() {
  server_pid >/dev/null || die "the server is not running; start it first"
  read_port
  mkdir -p "$OUT"
  AUTH=$FIXTURE_AUTH
  TOKEN=

  local fields='fields=Overview,PrimaryImageAspectRatio&enableImageTypes=Primary,Backdrop,Thumb&imageTypeLimit=1'
  api GET /System/Info/Public | save system_info_public
  api GET /Users/Public | save users_public
  api GET /QuickConnect/Enabled | save quick_connect_enabled
  local qc
  qc=$(api POST /QuickConnect/Initiate)
  SECRET=$(jq -er .Secret <<<"$qc")
  save quick_connect_initiate <<<"$qc"

  local auth
  auth=$(jq -nc --arg u "$ADMIN_USER" --arg p "$(admin_password)" '{Username: $u, Pw: $p}' |
    api POST /Users/AuthenticateByName -)
  TOKEN=$(jq -er .AccessToken <<<"$auth")
  save authenticate_by_name <<<"$auth"
  api GET /Users/Me | save users_me

  local views movies shows music
  views=$(api GET /UserViews)
  save user_views <<<"$views"
  movies=$(jq -er '.Items[] | select(.CollectionType == "movies") | .Id' <<<"$views")
  shows=$(jq -er '.Items[] | select(.CollectionType == "tvshows") | .Id' <<<"$views")
  music=$(jq -er '.Items[] | select(.CollectionType == "music") | .Id' <<<"$views")

  local list night field
  list=$(api GET "/Items?parentId=$movies&includeItemTypes=Movie&recursive=true&sortBy=SortName&sortOrder=Ascending&startIndex=0&limit=50&$fields")
  save items_movies <<<"$list"
  night=$(jq -er '.Items[] | select(.Name == "Night Signal") | .Id' <<<"$list")
  field=$(jq -er '.Items[] | select(.Name == "The Long Field") | .Id' <<<"$list")
  api GET "/Items/$night" | save item_movie
  api GET "/Items/Latest?parentId=$movies&limit=16&$fields" | save latest_movies
  api GET "/UserItems/Resume?mediaTypes=Video&limit=12&$fields" | save resume

  local shows_list series seasons season1 episodes pilot
  shows_list=$(api GET "/Items?parentId=$shows&includeItemTypes=Series&recursive=true&sortBy=SortName&startIndex=0&limit=50&$fields")
  save items_series <<<"$shows_list"
  series=$(jq -er '.Items[0].Id' <<<"$shows_list")
  seasons=$(api GET "/Shows/$series/Seasons?$fields")
  save seasons <<<"$seasons"
  season1=$(jq -er '.Items[] | select(.IndexNumber == 1) | .Id' <<<"$seasons")
  episodes=$(api GET "/Shows/$series/Episodes?seasonId=$season1&$fields")
  save episodes <<<"$episodes"
  pilot=$(jq -er '.Items[] | select(.IndexNumber == 1) | .Id' <<<"$episodes")
  api GET "/Items/$pilot" | save item_episode
  api GET "/Shows/NextUp?limit=12&$fields" | save next_up
  api GET "/MediaSegments/$pilot" | save media_segments

  local albums album tracks
  albums=$(api GET "/Items?parentId=$music&includeItemTypes=MusicAlbum&recursive=true&sortBy=SortName&$fields")
  save albums <<<"$albums"
  album=$(jq -er '.Items[0].Id' <<<"$albums")
  tracks=$(api GET "/Items?parentId=$album&includeItemTypes=Audio&recursive=true&sortBy=ParentIndexNumber,IndexNumber,SortName")
  save album_tracks <<<"$tracks"
  api GET "/Artists/AlbumArtists?parentId=$music&$fields" | save album_artists
  api GET "/Genres?parentId=$music&includeItemTypes=MusicAlbum&$fields" | save genres_music
  api GET "/Audio/$(jq -er '.Items[] | select(.IndexNumber == 1) | .Id' <<<"$tracks")/Lyrics" | save lyrics
  api GET "/Audio/$(jq -er '.Items[] | select(.IndexNumber == 2) | .Id' <<<"$tracks")/Lyrics" | save lyrics_unsynced

  local playlist collection
  playlist=$(item_id 'includeItemTypes=Playlist')
  api GET "/Playlists/$playlist/Items?$fields" | save playlist_items
  collection=$(item_id 'includeItemTypes=BoxSet')
  api GET "/Items?parentId=$collection&$fields" | save collection_items

  # As Patina asks at Auto quality, and at 2 Mbit/s, where its profile's
  # bitrates are the quality's too; direct play is turned off as well, since
  # the test files are below any quality.
  jq -nc --argjson profile "$DEVICE_PROFILE" '{DeviceProfile: $profile}' |
    api POST "/Items/$night/PlaybackInfo" - | save playback_info
  jq -nc --argjson profile "$DEVICE_PROFILE" --arg id "$field" '{
    DeviceProfile: ($profile | .MaxStreamingBitrate = 2000000 | .MaxStaticBitrate = 2000000),
    MediaSourceId: $id, MaxStreamingBitrate: 2000000, EnableDirectPlay: false, EnableDirectStream: false}' |
    api POST "/Items/$field/PlaybackInfo" - | save playback_info_transcode

  api GET "/Items?searchTerm=night&includeItemTypes=Movie,Series,Episode,MusicAlbum,Audio&recursive=true&limit=24&$fields" |
    save search_items
  api GET "/Artists?searchTerm=mira&limit=24&$fields" | save search_artists
  say "wrote $(find "$OUT" -maxdepth 1 -name '*.json' | wc -l) fixtures to $OUT"
}

# --- Main -------------------------------------------------------------------

[ $# -ge 2 ] || usage
command=$1
case $command in
  setup | start) use_dir "$2" create ;;
  *) use_dir "$2" ;;
esac
PORT=$DEFAULT_PORT
URL=http://127.0.0.1:$PORT
case $command in
  setup)
    [ $# -eq 2 ] || usage
    setup
    ;;
  start)
    [ $# -le 3 ] || usage
    PORT=${3:-$DEFAULT_PORT}
    valid_port "$PORT" || die "not a port: $PORT"
    PORT=$((10#$PORT))
    URL=http://127.0.0.1:$PORT
    start
    ;;
  stop)
    [ $# -eq 2 ] || usage
    stop
    ;;
  fixtures)
    [ $# -eq 3 ] || usage
    OUT=$3
    fixtures
    ;;
  *) usage ;;
esac
