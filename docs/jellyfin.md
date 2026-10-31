# Jellyfin client

Patina browses and plays a Jellyfin server's library inside the player
window: once a server is signed in, the idle player shows the library in
place of the welcome screen, and what it plays it plays in that same window.
This page is the design: what is in, how it fits Patina, and the rules the
code keeps. The API facts behind it were read from Jellyfin 12.4's source
and OpenAPI description and from 10.10.7's and 10.11.11's.

## Scope

In the first release:

- **Connecting.** A server by address, or found with Scan Network; signing
  in with a password or Quick Connect; several servers, one account on
  each; signing out.
- **Browsing.** Home (Continue Watching, Next Up, Latest in each library,
  My Media), each library, movies, shows with their seasons and episodes,
  collections, an item's detail page, and search; marking played or
  unplayed and favourite.
- **Playback.** Direct play of whatever mpv can open, the server
  transcoding only when Patina asks for a lower quality or the source
  can't be played directly; resuming where the server says; the server's
  default audio and subtitle tracks and external subtitles; versions;
  progress reported to the server; the next episodes queued in the
  playlist; skipping intros, credits, recaps and previews (media
  segments); the server's trickplay images in the seek bar's previews; a
  quality menu.
- **Music.** Albums, artists, playlists and genres, playing through
  Patina's music mode with the playlist as the queue, and lyrics.

Not in it: being a cast or remote-control target in other Jellyfin apps,
SyncPlay, Live TV, offline downloads, books and photos, server
administration, and approving other devices' Quick Connect codes. The
first two need a websocket connection to the server, which nothing here
does.

Servers from 10.10.7 on are supported: one code path covers 10.10.7,
10.11 and 12.x (the endpoints below are the same in all three).

## Where it lives

`src/jellyfin/` holds everything that talks to a server:

| File | What it does |
| --- | --- |
| `mod.rs` | The app-wide Jellyfin state: signed-in servers, the registry of what is playing, the image cache, the reporter. |
| `api.rs` | A blocking client for one server and account: the requests below, typed, on ureq. |
| `models.rs` | The server's JSON, as serde structs that take what they know and ignore the rest. |
| `auth.rs` | The `Authorization` header, address normalising, password and Quick Connect sign-in. |
| `store.rs` | `jellyfin.json` in the data folder (servers, accounts, the device id) and the tokens in the keyring. |
| `discovery.rs` | Scan Network: the UDP broadcast and its answers. |
| `images.rs` | Fetching, decoding and caching posters and backdrops, off the interface thread, each through the client of the server it comes from (`api::Clients`). |
| `playback.rs` | Choosing a stream, the device profile, what the load hook sets, mapping tracks, and reporting progress. |
| `segments.rs` | Media segments: which span plays, and whether to offer skipping it or skip it. |
| `trickplay.rs` | The seek bar's previews from the server's trickplay tiles. |
| `library.rs` | The library view's controller: pages, rows, navigation, search. |

`ui/library.slint` is the library view, drawn in the player window where
the welcome view is, with its pages as components.

Network requests run on worker threads, never on the interface thread,
and their results come back through `async_channel` and
`slint::spawn_local`, with a request counter so that a stale answer (from
a page the user has left) is dropped, as `search_online` does. The
library's page loads, a playing item's segments and its trickplay tiles
share six threads (`session::load`), so that a server that stalls while
the user clicks on fills those six rather than a thread per request until
each times out; a load whose page was left meanwhile skips its request.
The keyring has one thread of its own, which does what it is asked in
order (`session::keyring`): a token saved and then deleted, by signing out
while the keyring waits to be unlocked, must not be deleted first and
saved after.

## Connecting

**Addresses.** What the user types is normalised: `https://` is assumed
when no scheme is given, port 8096 when the scheme is http and none is
given, and a path is kept as the server's base URL. Patina asks
`/System/Info/Public` at that address, following redirects, and keeps the
address it ends at together with the server's id, name and version. When
https was only assumed and what answers speaks plain http, as most servers
at home do, it is asked again over http at the same port, and signing in
says that the address is plain http; when the TLS handshake fails, the
message suggests typing `http://`. API
requests after that follow a redirect only to the same server (scheme,
host and port), with the header; a redirect anywhere else is an error
rather than a request without credentials, or with them sent to another
server.

**Scan Network** broadcasts `who is JellyfinServer?` to UDP port 7359 and
lists the servers that answer within two seconds, with their addresses.
A server in a container often reports an address only the container can
reach, so the address the answer came from is offered too. An answer
proves nothing (anything on the network can send one), so a server found
this way is only filled in; the user still connects to it.

**Signing in.** `/Users/Public` lists the accounts a server shows; the
user picks one or types a name, then a password (`/Users/AuthenticateByName`),
or uses Quick Connect: `POST /QuickConnect/Initiate` gives a six-digit
code to approve in another Jellyfin app, `/QuickConnect/Connect` is asked
every five seconds until it is, and `/Users/AuthenticateWithQuickConnect`
signs in. A request expires after ten minutes.

**The header.** Every request carries

    Authorization: MediaBrowser Client="Patina", Device="…", DeviceId="…", Version="…", Token="…"

with the keys spelled exactly so (the server compares them case by case)
and the values percent-encoded (it URL-decodes them, so `+` would become
a space). The token is never put into a URL. The legacy headers and the
`api_key` parameter are off by default from 12.0.

- `DeviceId` is random, made once per Patina configuration and kept: a new
  sign-in revokes the same user's other tokens on the same device id, so
  two installations must not share one.
- `Device` is "Patina on Linux", "on macOS" or "on Windows", and must stay
  the same, since the server overwrites what it stored when it changes.

**Keeping it.** `jellyfin.json` keeps each server's id, name, address,
optional CA file and the account's id and name. The token goes into the
keyring, under the service "Patina Jellyfin" and the account
`<server id>/<user id>`. Without a keyring the token lives only as long as
Patina runs, and the next start asks to sign in again. A 401 from a server
means the token is gone (signed out elsewhere, the password changed, the
device removed): Patina forgets it and asks again. Signing out stops
what any window plays from that server, after reporting where it
stopped, then calls `/Sessions/Logout` (once the reports on their way
have gone, or five seconds have passed, since they carry the same token)
and deletes the token. Playing on would report nothing more, and the
episodes queued after it couldn't open.

**Certificates.** Requests trust what the system trusts (ureq's platform
verifier). A server can also be given a CA file, whose certificates are
then trusted for that server. A server's own self-signed certificate can
be its CA file, which pins it: it then has to be given again when the
certificate changes. That works for a certificate made for the server;
one made as a CA (as `openssl req -x509` does by default) is refused when
the server presents it, and such a server needs a small CA of its own,
whose certificate is the file. Over plain http
the password and the token travel unencrypted; Patina allows it and says
so when signing in. Playback sets mpv's `tls-verify` for the file and its
`tls-ca-file` when the server has one, so that the stream's requests are
checked as the API's are.

## Browsing

The library view replaces the welcome view in an idle player window while
any server is signed in. Its bar has Home, the account's libraries, Search
and This Computer, which is the welcome view's Open, Open URL and Recent.
Pages stack: opening an item pushes its page, Back (the button, the mouse's
back button, Backspace) pops it. What plays from the library plays in its
place; Escape (outside full screen, where it does nothing as bound by
default) or Stop goes back to it, and so does the end of the playlist's
last entry, rather than mpv's last frame. The window takes back the size
it had in the library, which fitting it to a small video or a cover
changed. Signing in, the keys typed where no field has the keyboard are no
shortcuts (a name typed there would quit at its first "q"), and the user
name field has it even with the server's accounts listed.

| Page | Requests |
| --- | --- |
| Home | `GET /UserItems/Resume?mediaTypes=Video`, `GET /Shows/NextUp`, `GET /Items/Latest?parentId=…` per library, `GET /UserViews` |
| A library | `GET /Items?parentId=…&includeItemTypes=…&recursive=true&sortBy=…&startIndex=…&limit=…`, in pages as it scrolls |
| A show | `GET /Shows/{id}/Seasons`, `GET /Shows/{id}/Episodes?seasonId=…` |
| An item | `GET /Items/{id}` (every field: media sources, chapters, people) |
| Music | albums, `GET /Artists/AlbumArtists?parentId=…`, playlists, `GET /Genres?parentId=…&includeItemTypes=MusicAlbum`; an album's tracks sorted `ParentIndexNumber,IndexNumber,SortName`; `GET /Playlists/{id}/Items` in the playlist's own order |
| Search | `GET /Items?searchTerm=…` for movies, shows, episodes, albums and songs, and `GET /Artists?searchTerm=…` |
| Played, favourite | `POST`/`DELETE /UserPlayedItems/{id}`, `POST`/`DELETE /UserFavoriteItems/{id}` |

Queries name the item types and `recursive=true` (an untyped library query
is slow on a large library) and the image types they will show. Grids are
rows of posters in a `ListView`, loaded a page at a time. Lists whose rows
are made all at once (an album's or playlist's songs, a collection's
items, an artist's albums) stop at 300, since thousands of rows held the
interface up for seconds; playing the page still queues up to 1000.

**Images.** `GET /Items/{id}/Images/{type}?tag=…&maxWidth=…&format=Jpg`,
with widths rounded to a few fixed sizes (the server caches each size it
makes) and no `fill` parameters. A worker pool fetches and decodes them
into `SharedPixelBuffer`s, which become `slint::Image`s on the interface
thread; they are kept in memory and on disk under the data folder's
`jellyfin/images`, with the least recently used removed past the size
Preferences › Jellyfin sets (256 MB unless changed), where Clear Poster
Cache empties it. Until an image arrives its blurhash, decoded small,
stands in for it. Posters show what a library holds, so signing out of a
server, or declining to sign in to it again, clears the cache: every
server's images together, since the files are named by a hash of their
URL; the other servers' come back as they show.

## Playback

**What the playlist holds.** A Jellyfin item is opened as
`jellyfin://<server id>/<item id>`, which identifies it without a token
and without depending on the stream the server chooses. That is what mpv's
playlist, Patina's history, Recent and the title bar see. Patina's load
hook (which runs before mpv opens anything) resolves it: it asks the
server for the item (every time, for the resume position as it is now and
for what the media controls show) and its playback info, picks the stream
and sets mpv's `stream-open-filename` to it. The same entry therefore
reopens after a restart, from History or Recent, and resumes where the
server says. While it waits for the server the window says so, with a
Stop button. An entry that fails for want of a sign-in has the library
ask for one, saying why. An entry that a playlist file lists (mpv's `playlist-path`
is set) is refused: only what Patina's own library, History and Recent
open is asked of a server with the account's token, never what a file
from elsewhere names.

**Choosing a stream.** `POST /Items/{id}/PlaybackInfo` with a JSON body
holding the device profile, the media source if the user chose a version,
the audio and subtitle stream if chosen, and the quality's maximum
bitrate. The profile direct-plays everything (`DirectPlayProfiles` of
type Video and Audio with no containers or codecs, which means any) and
transcodes to HLS in MPEG-TS (H.264 or HEVC video, AAC, AC-3, E-AC-3 or
MP3 audio) when it must. The answer's `PlaySessionId` goes with every
report.

The preferences can rule out what mpv could play but the computer may not
decode well, as `CodecProfiles` whose conditions a stream must meet to
play directly: HEVC and AV1 each by a codec profile with the condition
`Width <= 0` (required, so that a stream of unknown width fails it too),
HDR by `VideoRangeType` being one of `SDR` and `DOVIWithSDR`, and anything
over 1080p by `Width <= 1920` and `Height <= 1080`. The server then
transcodes such a stream, to H.264 alone when HEVC is ruled out. They
apply from the next file that opens.

- Direct play: `{address}/Videos/{id}/stream.{container}?Static=true&mediaSourceId=…&deviceId=…&Tag=…`
  (`/Audio/…` for audio). It needs no token, and supports seeking.
- Transcode: the source's `TranscodingUrl`, with its `ApiKey` parameter
  removed, since the header carries the token, and `EnableTrickplay=false`
  added: the server writes the token into the master playlist's trickplay
  line otherwise, and Patina takes trickplay from the API instead.

**What the hook sets** for a Jellyfin entry, as file-local options so
that they end with the file:

- `stream-open-filename`: the stream.
- `http-header-fields`: the `Authorization` header, for a transcode only.
  Its HLS playlists and segments need the token; the file itself and the
  server's subtitle files are served without one (10.10.7's and
  10.11.11's OpenAPI descriptions agree with 12.4 on that), and every way
  the header could go astray is one less for the common case. While the
  file plays mpv sends it to every host it contacts, so only the server's
  own subtitles are added, never a third-party subtitle URL, and a
  `TranscodingUrl` on another host isn't played. With the header go
  `max_redirects=0`, added to the user's own `stream-lavf-o` (FFmpeg
  would send the header on to wherever a redirect points, plain http
  included), and `prefetch-playlist` off (mpv would open the next entry
  with this file's options while it plays).
- `start`: the server's resume position, unless the user chose to play
  from the beginning (which holds for that one load: the entry opened
  again from History, Recent or the playlist resumes where the server
  says), and `none` otherwise. mpv reads a watch-later file before the
  load hook runs, so `start` is always set, and none is written for a
  Jellyfin entry, playing or still opening; `save-position-on-quit` is
  off. An item watched before keeps its played mark when it is watched
  again and stopped part-way through; it resumes there all the same, as
  Continue Watching lists it, and its card shows both.
- `aid` and `sid`: the server's audio and subtitle (see Tracks).
- `force-media-title`, `ytdl` off, `tls-verify` and `tls-ca-file`.
- The external subtitles (`sub-files`), each from its `DeliveryUrl` with
  `ApiKey` removed.

The hook runs on its own thread already, so the request there may block.
mpv takes a stop, or another entry, only once the hook is done, so the
requests run on a thread of their own while the hook looks five times a
second whether mpv is still on the entry; when the user has stopped or
moved on, it gives up and what the server says later is dropped. A
server that takes the connection and never answers would otherwise hold
the window until the request timed out (30 seconds), entry after entry.

**Tracks.** Jellyfin numbers every stream of a source, external ones
first; mpv numbers each kind from 1, external ones after the embedded
ones. Patina maps the server's default audio and subtitle streams to mpv's
`aid` and `sid` by counting embedded streams of each kind in order, and
external subtitles by the order they were added, and the hook selects
them as file-local options: set while the file plays, mpv would keep them
for the next file (subtitles off for a local file after an item played
without), and the start report would name the tracks before them. Once
mpv lists the file's tracks the window checks the selection, knowing an
external subtitle by the URL mpv loaded it from (`external-filename`),
since one that failed to load moves the others' numbers, and corrects it
where mpv doesn't have it as the server said; the menus name the server's
subtitle files and report them the same way. What the window or the user
selects while the file plays ends with it too. Choosing a subtitle the
server must burn in (delivery `Encode`) asks for playback info again and
reloads at the same position. A transcode carries one audio stream, which
the Audio menu names; it lists the source's other audio streams after
mpv's track, and choosing one has the server transcode it instead.

**Reloading.** A burned-in subtitle, another audio stream for a
transcode, another quality or another version loads the entry again where
it was: the window gives the hook the position and the audio and subtitle
streams playing now, for that load only, so that they play on (another
version asks for none, since its streams are numbered its own way, and
the server chooses them as it would). The entry played again later starts
from the streams chosen on its page or the server's. The version chosen
in the player is the entry's, until the library opens it again with its
page's; the quality is the window's, for every item it plays. Spans of
the item skipped without asking stay skipped across the reload.

**Reporting.** A reporter thread sends `POST /Sessions/Playing` once the
file has begun playing (mpv's first playback restart, by when it knows
where it is, whether it can seek and which tracks play; a file stopped
before that is reported stopped and failed, which changes nothing of the
account's), `POST /Sessions/Playing/Progress` every ten seconds
(paused too: the server stops a session it hasn't heard from in five
minutes, and a transcode after a minute) and at once on pause, resume,
seeks and track changes (changes within a second making one report), and
`POST /Sessions/Playing/Stopped` when the file ends or the window closes.
mpv keeps a file open at its end, which for the server is where playback
stopped, so the stop goes when the file reaches its end; a seek back from
there plays on, reported as a new start. A progress report still queued
when a later one of the same playback comes is dropped, so a slow server
hears the latest position rather than every one since. Positions are in
ticks, ten million a second. Quitting waits up to two seconds for the last
report. The server decides from the reports what counts as watched and
where to resume; a report answered 401 signs the account out, as any
request does. Jellyfin items write no watch-later files: the server keeps
their place. The playlist's last entry reaching its end stops it, and the
window shows the library again once the server has taken the report.

**Next episodes.** Playing an episode adds the rest of the season, and
the seasons after it, to the playlist as `jellyfin://` entries, so mpv
plays on as it does a folder, unless the account has turned off playing
the next episode automatically. The playlist shows their titles. Each
resumes where the server says when it opens, since the hook asks for the
item then.

**Segments.** `GET /MediaSegments/{id}` gives the intro, outro, recap,
preview and commercial spans, when the server has a plugin that finds
them (12.4 has none built in, and leaves out segments from a plugin no
longer installed). It is asked once a video has opened, on a load
thread, and its answer is dropped if another file has started since.
Preferences › Jellyfin has a choice for each kind: a Skip button (the
default for intros, recaps, previews and credits), skipping without
asking (the OSD says "Skipped Intro"), or nothing (the default for
commercials).

While a span plays Patina shows Skip Intro, Skip Credits, Skip Recap,
Skip Preview or Skip Ad at the bottom right of the video, beside the
controller where there is room and above it otherwise: for five seconds
as the span begins, and again while the controller shows or after a key,
since in a small window it would cover the subtitles mpv draws there; not
while the seek bar's preview shows, which it would cover. It seeks to the
span's end, exactly, since a keyframe before the end would be in the
span again, and the OSD says what was skipped; credits that run to the
end end the file, and the playlist plays on. Tab reaches the button and
Enter or Space presses it, and the key binding action `skip-segment`
(S by default, which the button shows) does the same. A span is skipped
without asking once per playback, and not before the file has begun
playing: a seek back into it offers the button instead, since the user
went back for it. Of spans that overlap, the one that began first counts,
and the button goes in the last second of a span and for spans under a
second long.

**Trickplay.** When the server has trickplay images for the source (the
item's `Trickplay` field, by media source and width), the seek bar's
previews come from those tiles
(`GET /Videos/{id}/Trickplay/{width}/{index}.jpg?mediaSourceId=…`), the
narrowest at least twice the preview's width. Patina never makes its own
thumbnails of a stream, so without trickplay images a Jellyfin item's
preview shows the time and chapter alone. A tile is a grid of thumbnails,
one every `Interval` milliseconds; the last holds only those left, and
its image may be smaller than the grid. Only a signed-in request gets
one, so each is fetched with the account's client on a load thread and
decoded there; two are kept decoded, and the thumbnail under the pointer
is cut out of its tile. A tile that failed isn't asked for again for half
a minute. Nothing of them is kept on disk.

**Quality.** The Video Quality menu has Automatic, which plays directly
(or transcodes when the server must, or the preferences rule the source
out), and those of 20, 10, 6, 4 and 2 Mbit/s that are below the source's
bitrate, which ask the server to transcode to that bitrate; the item
checked says whether the original or a transcode plays. Changing it while
playing reloads at the same position, and it holds for every item the
window plays after: lowered for the network, it is wanted for the next
episode too. The Video menu has it, the tracks and Version at its top,
since in a window fitted to a small video the menu is cut off at its
edge.
Preferences can make Patina ask for a transcode of HEVC, AV1, HDR or
anything over 1080p, for machines that can't decode them (see Choosing a
stream).

**Versions.** An item with several media sources plays the version chosen
on its page (the registry's `media_source_id`), or the server's choice.
While it plays, the Video menu's Version submenu lists its versions and
loads another one where it was.

## Music

Albums, artists, playlists and genres are pages of the music library. An
album or playlist plays as a playlist of its tracks, which is the queue:
Play Next and Add to Queue insert into it. Tracks play through the same
`jellyfin://` entries and hook, in Patina's music mode (whether an item is
audio is the server's to say, since the stream is remote, which Patina
never counts as music by itself). Gapless playback
needs mpv's playlist prefetch, which per-file options defeat, so tracks
play one after another with mpv's usual gap. Lyrics come from
`GET /Audio/{id}/Lyrics` and show in music mode's drawer, the current line
highlighted when they are timed.

## Keeping the token secret

The token is in the keyring, in memory, and in the header of requests to
its server. It is not in any URL Patina stores or shows, so not in
History, Recent, the playlist, Copy URL, MPRIS, the Inspector's `path`,
or logs. The server writes it into the URLs it hands out (a transcode's,
a subtitle's); the models remove it as they read them, and a transcode
asks for no trickplay rendition in its HLS playlist, whose URI 12.4 would
write the token into. The structs that hold a token print it as hidden.

mpv gets the header for a transcode only (see What the hook sets), and
never one that a redirect, the next entry or a transcode on another host
would take elsewhere. mpv's wrapper puts the value it failed to set into
its error messages, so the header goes to mpv, as a list, through a
setter that leaves the value out. mpv itself logs every property it
sets, value and all, at its verbose level, which `--verbose` shows; while
that setter sets the header, mpv's own messages go no further than info.
mpv's log file (`log-file`, from the preferences' extra options, mpv.conf
or `--mpv-log-file`) records the debug level whatever the levels say,
which no option lowers: while mpv writes one, a transcode doesn't play,
and says why. At its trace level FFmpeg prints each request with its
headers, so while the header goes with a file FFmpeg's messages stop at
debug. The Inspector's Watch tab shows `http-header-fields` as hidden.

## Tests

`tools/jellyfin-server.sh` downloads a pinned Jellyfin server (the
official portable build, which carries its own .NET) and jellyfin-ffmpeg,
makes a small library with ffmpeg (movies, a show with two seasons and
subtitles, an album), starts the server on 127.0.0.1 with all its state in
one folder (its discovery and UPnP off, its outgoing requests sent
nowhere), completes its setup through the API with a random admin
password, and waits for the scan. `start` prints the variables the tests
read. The Jellyfin tests run against it when `PATINA_TEST_JELLYFIN_URL`
names it, as CI's Linux job does; without it they are skipped, unless
`PATINA_TEST_REQUIRE_JELLYFIN` is set. Checks that only hold for the
generated library run when `PATINA_TEST_JELLYFIN_LIBRARY` is `generated`,
so the suite can also run against a real server. The test server has no
media segments, which 12.4 makes only with a plugin, so what Patina does
with them is checked by unit tests of the timing alone.
