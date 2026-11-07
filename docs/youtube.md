# YouTube section

Patina has a YouTube section in the player window, opened from the welcome
screen: a feed of the channels the user follows, search, a Watch Later list,
the videos played and downloads. There is no account. Patina never signs in to
Google, and its lists are its own: they don't appear on YouTube, and the
interface says so where they show. This page is the design as built: what is
in, what leaves the machine, and the rules the code keeps.

## Scope

In the first version:

- **Feed.** The newest videos of the channels followed, newest first, shown
  from what was saved and updated behind that. Shorts are hidden, with a
  switch to show them. Live and upcoming videos have a badge where that is
  known.
- **Search.** Videos and channels, on Enter and never while typing.
- **Watch Later.** Saved from any card, or from the File and right-click
  menus while a YouTube video plays, and played from the list.
- **Channels.** Added by a pasted link or `@handle`, from search results or
  a channel's page, or by import; a small page for each with its videos and
  Subscribe or Unsubscribe.
- **History.** The YouTube videos in the player's history, newest first,
  with Clear History.
- **Downloads.** From cards, through the Downloads window, into the
  Preferences download folder.
- **Import and export.** Both lists, in the files Google's data export uses,
  and the channels in the file NewPipe exports its subscriptions to.

Not in it: an account or sign-in of any kind, Google's official API, browser
cookies, servers of other YouTube front ends, any request of Patina's own
beyond channel feeds and pictures (everything else is yt-dlp's), comments,
live chat, recommendations, YouTube's own history, playlists as lists (other
than the one Watch Later), YouTube Music, a filter for what search finds,
batch downloads, and updating yt-dlp from inside Patina.

## YouTube's terms

YouTube's terms bar downloading and automated access, along with getting
around their technical measures, except for what YouTube allows or permits
in writing. This section does those things: yt-dlp reads YouTube's pages and
its internal player interface, and fetches and saves videos, and the feed
reads channel feeds by program. Having no account means there is nothing to
suspend, but YouTube can refuse an address, and Patina names that when it
happens rather than working around it. This page states the fact and gives
no legal advice. Using the section is the user's own choice, and nothing
else in Patina depends on it.

Patina isn't affiliated with YouTube or Google. The entry is the plain word
"YouTube" with a generic icon from Lucide, and no logo of theirs is in the
repository.

## What leaves the machine

**When.** Nothing goes at Patina's startup, on the welcome screen or in the
background. `Session` asks the network nothing until a window opens the
section, no timer checks for anything, and leaving the section stops what is
on its way (a running yt-dlp is killed, with whatever it started; a feed
request already sent finishes within its limit). What that left undone waits
for the next time the section opens, and nothing starts behind a section no
window shows. In the section, requests start from:

- **Opening it, and coming back to it from a video:** the feed, if it was
  last checked over 30 minutes ago (an hour after YouTube refused), for the
  channels that haven't answered in that time, any channel never asked, and
  the titles of saved videos that have none.
- **What the user does:** Update Now, a search on Enter, adding a channel,
  opening a channel's page, importing a list (its new channels are fetched,
  and its Watch Later rows' titles), playing and downloading.
- **What is on screen:** thumbnails and channel pictures for the lines in
  sight and two screens' worth each way.

Opening the section counts as asking for the first of those. It is spelled
out here because a list kept in Patina still shows itself to Google each
time it is brought up to date.

**What each request sends.** All of them go from the user's network address,
or the proxy's when one is set (see Proxy).

| Source | When | To | What it says |
| --- | --- | --- | --- |
| Channel feed (RSS) | Each channel, first, in a feed update | `www.youtube.com/feeds/videos.xml` | The channel's id, one request per channel, with `Patina/<version>` as the user agent, and no cookie |
| yt-dlp, listing | Channels the feed didn't answer for, and a channel's page | `www.youtube.com` | The channels' ids, up to 8 in one process |
| yt-dlp, search | Enter in the search field | `www.youtube.com` | The search text, at least twice: once for videos, once for channels |
| yt-dlp, adding a channel | Adding a channel | `www.youtube.com` | The `@handle`, old name or id that was pasted, as an address Patina builds |
| yt-dlp, details | A saved video with no title, one every 15 seconds | `www.youtube.com` | The video's id |
| yt-dlp and mpv, playing | A video opens | `www.youtube.com`, then `googlevideo.com` for the stream | The video's id to yt-dlp; mpv then asks for the signed stream addresses yt-dlp returned, which carry the user's address |
| yt-dlp, downloading | A download starts | The same | The video's id, then the stream |
| Thumbnails | A card near the screen, unless cached | `i.ytimg.com` | The video's id, in `/vi/<id>/mqdefault.jpg`, and `hq720` or `hqdefault` as well as or instead of that, with `Patina/<version>` and no cookie |
| Channel pictures | A channel's row near the screen, unless cached | `yt3.googleusercontent.com`, `yt3.ggpht.com` | An address from yt-dlp's answer, the same way |

Patina's own requests, the feeds and pictures, carry only `Host`, `Accept`
and that user agent, which a test holds them to.

yt-dlp identifies itself as its own clients do, not as Patina. It asks
YouTube's own interfaces as it sees fit, and that can change with its
version. It gets none of the user's yt-dlp options and `--ignore-config`
keeps its config file out, so no cookie of the user's goes with it; what
YouTube sets during a run lasts for that run and, in the player, that file.
The one thing it takes from the preferences is the proxy. Calls to play or
download also carry the fixed client argument that architecture.md's Online
video describes.

**What Google can tell from that.** No account doesn't mean no trace. An
update asks about each channel in turn, so Google can see from one address
which channels are followed. It sees each search and each video played or
downloaded, and thumbnail requests show what scrolled by, Watch Later's
included. It can tie all of these together by the address. A proxy or VPN
moves that from the user's address to the proxy's.

**What doesn't leave.** The lists as files, whatever is imported (a chosen
file is only read here), Watch Later's added-at times, watched marks and
progress, download paths. A video's progress isn't reported anywhere; the
player's stream requests are all YouTube sees of a viewing. Nothing goes to
Patina's developer, or to anyone but YouTube and a proxy the user set, and
there is no telemetry.

**Online video off.** "Play videos from websites with yt-dlp" off stops
every yt-dlp use. A section opened with it off still updates from the
channel feeds and fetches thumbnails, as those aren't yt-dlp; the section
then says online video is off.

**A YouTube address opened some other way** (Open URL, a `patina://` link)
is not the section's. It goes through mpv's hook with the user's yt-dlp
options and config, as any web address does, and those can hold cookies.
Only its watched mark and the Watch Later menu item are shared with the
section. Recent, Resume Last Playback and the History window open an entry
the way it last went, which the entry keeps: a video the section sent is
still the section's, with none of those options, and goes back to the section
if that is showing; the same address from Open URL still has them.

**On this machine.** In the data folder, `youtube.json` holds the lists,
`youtube-feed.json` the saved feed and `youtube-images` the thumbnails of
what was on screen, search results included. Clear Recent clears watched
marks and nothing else of these, Clear Saved Positions clears where videos
were left off and keeps what was seen of them, the History page's Clear
History clears the marks and the YouTube videos in the history (see History),
and the thumbnails' folder has no button: it can only be deleted by hand.
Searches aren't kept or logged, though the text is on yt-dlp's command line
for as long as it runs, where a process list can show it, as with any
program's arguments. Errors from the section's own yt-dlp calls have the
search text, stream addresses and a proxy's password cut out before they are
shown or kept (`ytdlp::scrub`), and `player/mod.rs` cuts stream addresses out
of what mpv logs.

The Flatpak has every permission this needs already: it asks for no new one.
Its metainfo's comment on the content rating sums this up, and the site's
privacy page has to agree with this section (see development.md).

## Where it lives

`src/youtube/` holds the section apart from any window:

| File | What it does |
| --- | --- |
| `mod.rs` | The checked `VideoId` and `ChannelId`, the addresses built from them, reading what was pasted (`ChannelInput`), cleaning others' text, `Video` and its merge, failures by name (`Failure`), and `Cancel`. |
| `store.rs` | `youtube.json`: channels, Watch Later, marks, download paths and the Shorts switch. |
| `feed.rs` | The `Source` trait, `build` (the circuit breaker, batches, what ends the asking) and `SavedFeed`, the file `youtube-feed.json`. |
| `rss.rs` | Reading a channel feed, as a `Source`. |
| `ytdlp.rs` | Running yt-dlp: listings as a `Source`, search, resolving a channel, a video's details, its version and age, and its errors by name. |
| `client.rs` | Plain requests to YouTube's hosts and no others, for feeds and pictures. |
| `takeout.rs` | Reading and writing Google's files, and telling NewPipe's apart. |
| `newpipe.rs` | Reading and writing NewPipe's subscriptions file. |
| `session.rs` | The app-wide state on the interface thread: the store, the feed, the threads requests run on, the picture cache, notices, and the player's history as the History page reads it. Windows follow it as `Listener`s. |
| `view.rs` | `YouTubeController`, one per window: pages, cards, images near the screen, messages. It fills the `YouTube` global. |
| `playing.rs` | Time seen, where a video picks up, which part of a list plays, and the default quality. |
| `fixtures/` | Made-up XML, JSON and error text for the tests, NewPipe files among them. |

`ui/youtube.slint` draws the bar, the message under it and the pages,
`ui/youtube-card.slint` the cards and channel rows, and
`ui/youtube-model.slint` holds the `YouTube` global and the structs the
controller fills. The window's side is in `src/window.rs` (`from_section`,
`section_load`, `Sitting`, the return to the section) and the queue's in
`src/downloads.rs`.

Requests run on threads other than the interface's, and answers come back
through `async_channel` and `slint::spawn_local`. A search, a channel's page
or adding a channel can each be cancelled, and a cancelled one's answer is
dropped.

## The lists

`youtube.json` is the only copy of what the user built up, so it is written
whole or not at all (a temporary file, synced and renamed) and never
silently replaced. A file that can't be read whole, or that a newer Patina
wrote, is copied beside itself as `youtube.json.bad` first, what can be read
is kept row by row, and a message under the section's bar says so and where
the copy is. A file that can't be opened at all, or copied, stops saving
until it can, and that is said too. A change that couldn't be saved stays in
memory and is said.

Lists hold at most 5,000 channels and 20,000 videos, far past real use,
which bounds what a hostile import can make Patina fetch and draw. Marks
stop at 20,000, the oldest going first.

A video on Watch Later keeps when it was added, so that an export can give
it back. It stays on the list if YouTube says it can't be watched, marked as
such, in case that changes. Saved videos are newest-added first.

## Import and export

**Reading.** The user picks a file, which is read once, into memory, off the
interface thread, and never opened in place or kept open. Only a plain file
of at most 32 MB is read: a pipe or a device could hang. A zip is refused
with a message to unpack it first, as the whole export comes as one.

Columns are found by header name, not position: `Channel Id` and `Channel
Title`, or `Video ID` and `Playlist Video Creation Timestamp`. A byte-order
mark is passed over, UTF-16 with one is read (a spreadsheet can save that),
and blank lines don't count. A file whose header isn't known is taken only
if its rows plainly have one of those shapes, in case another language's
export translates the names; this is a guess, since it hasn't been seen.
Half its rows having a channel's id will do, as those can't happen by chance.
Any word of eleven letters looks like a video's id, so that shape takes nearly
every row fitting, with timestamps beside them.

The file name is not used. Any playlist's `<name>-videos.csv` has Watch
Later's columns and so joins Watch Later, as that page says while it is
empty. `playlists.csv`, which lies beside them, is refused by its `Playlist
ID` column or what that column holds, though a title of eleven letters would
otherwise look like a video.

Nothing can stop a read once it has begun, so a wrong file has to be quick.
Rows past 40,000 are only counted, as skipped, which keeps it to seconds and
the memory to what a list needs. The CSV is read a field at a time, and only
a row's first 32 fields are kept, so a wrong file of one long row of commas
costs no more than a list either.

**Nothing in a file is trusted.** A channel's id is `UC` and 22 id
characters, a video's 11 of them, and that can begin with `-`. Every address
is built again from the id, and the file's `Channel Url` is never read. A
row with a bad id is counted and skipped, and titles are cleaned of control
characters and direction overrides. The whole file goes through the CSV
parser, so a quoted title can hold a line break. A quote left open would
swallow the rows after it, and an odd number of them says that happened, so
that file goes line by line instead, and one bad row never loses the import.
Two stray quotes are valid CSV, and what lies between them reads as one title,
which shows on the list.

**After.** The section says how many were added, how many were there
already, and how many were skipped, the newest such message first. New
channels are fetched. Watch Later's rows have no titles in Google's file, so
they show as "Fetching its title…" and can be played at once, while titles
come one at a time, the newest added first as they are at the top of the list,
15 seconds apart. Rows imported meanwhile are fetched after those. A row gets
three tries, unreadable answers included, and then shows as "Untitled video",
as it does while nothing is fetching; importing it again gives it three more.
A video YouTube says can't be watched is marked and left. A block, the network
or two time limits in a row stop the fetching, which picks up the next time
the section opens. It also stops when a video starts playing, since the two
count against the same limit on a guest, and is taken up again on coming back.

**NewPipe's file.** Import takes the JSON file NewPipe exports its
subscriptions to through the same button, the same read and the same report.
It is told from CSV by what it holds, a brace or bracket first, and not by its
name. It always means channels: NewPipe has no form of Watch Later here. Its
`app_version` fields are ignored, as NewPipe itself ignores them, and a JSON
file without a `subscriptions` list is refused as not NewPipe's. Each entry's
`service_id` must be 0, NewPipe's number for YouTube; SoundCloud, PeerTube,
Bandcamp and media.ccc.de entries are counted as skipped. Only a `url` with the
channel's id in it can be used: `/channel/UC…` on `youtube.com`, `www.`, `m.`
or `music.`, over http or https or with no scheme at all, with any slash or
query after, or the id alone. It is read as an address typed into Patina is,
and only for the id. An `@handle`, `/c/` or `/user/` address would need a
request to YouTube to become an id, and reading a file asks nobody, so those
are skipped too, as is an entry with a missing or wrongly typed field. A
missing name only leaves the title to be fetched. As with Google's files, the
address is built again from the id, the name is cleaned, and entries past
40,000 are only counted. Only an entry's three fields are kept, and anything
else in it is stepped over, so one huge field costs no more than the file.

**Writing.** Export writes the same columns: `subscriptions.csv` with
Google's own `http://www.youtube.com/channel/<id>` addresses, and `Watch
later-videos.csv` with the added-at time as `YYYY-MM-DDTHH:MM:SS+00:00`. The
Channels page's Export is a menu, As in a Google export (CSV) or For NewPipe
(JSON), so the form never rests on a filter in the dialog. NewPipe's file has
NewPipe's keys and no others, since NewPipe refuses a key it doesn't know, on
one line: for each channel `service_id` 0, `url`
`https://www.youtube.com/channel/<id>` and `name` its title, empty if it
hasn't been fetched, then `app_version` `0.29.1` and `app_version_int` `1015`
(a real release, which NewPipe doesn't check). The list comes first, as 0.29.1
writes it; older releases put it last, and NewPipe reads the keys in any
order, as Patina does. Export writes straight to the file chosen, as the
sandbox's portal lets Patina at that file alone.

## The feed

`feed::build` asks two sources in turn, and neither can make the interface
wait.

**Channel feeds first.** `youtube.com/feeds/videos.xml?channel_id=` gives 15
entries with exact dates, and Shorts told apart by their `/shorts/` links;
no length and nothing about live streams. Three requests run at once. For
hours at a time the feed server answers 404 or 500 with an HTML page, for
channels that are fine, so such an answer means nothing about the channel.
**A 404 never means a channel is gone.** The XML parser refuses a document
type declaration, and an entry is only taken for the channel asked about.

**A circuit breaker.** After 3 answers in a row that say nothing of the channel
(not a feed, a network failure, a time limit, the proxy), no more are asked in
that refresh. Once the feed server has answered properly it takes 9 in a row,
as an old list holds channels whose feeds always answer 404, side by side as
often as not, and they say nothing of the server. Those channels, the ones not
yet asked and any other the feed failed on go to yt-dlp. A list that begins
with three such channels trips it at once, which costs more requests and gives
rougher dates and nothing else.

**yt-dlp as the main path.** During an outage this is what runs. One process
lists up to 8 channels with `--flat-playlist -J --playlist-end 30`, two run
at once, each waiting a second before its next, as starting yt-dlp is most
of a call's time and one address has a budget of about 1,000 page requests
an hour. `-J` prints one line per address, `null` for one that failed, and
exits 1 even though the others are fine, so lines go to channels by position
and the exit code means nothing. Its listing has lengths and live state
where the feed has dates, but only rough dates of its own, and `/videos`
leaves Shorts out. The channel feeds still run without yt-dlp, whose failure
is then named on the channels they didn't reach.

**One channel's failure stays its own.** A channel keeps what is known of
it, and its state records when it last worked and what went wrong. Failures
that would be the same for everyone end the asking at once rather than
repeating for hundreds of channels: a block, no yt-dlp, online video off; a
whole batch failing on the network, a time limit, the proxy or an outdated
yt-dlp does too. No new process starts after that, though one already started
finishes, and the channels not asked take that failure. The Channels page
shows when each last succeeded and what stopped it, in yt-dlp's words when
Patina has no name for it, and the message under the bar names the commonest
failure and how many channels it stopped. A channel YouTube won't show says
so on its row and doesn't count towards that.

**A channel that is gone.** Only yt-dlp passing on YouTube's "does not
exist" can mark one, and it is only marked: it stays on the list until the
user removes it, and the mark lasts until a later answer says anything else.

**Merging.** A new answer never loses what is known (`Video::absorb`). An
exact date beats an approximate one whichever came first; of two approximate
ones the older stays, since "4 days ago" only grows coarser. Videos sort
newest first, with when one was first seen standing in for a missing date,
then by id, so that the order never shifts by itself. A video no longer in
an answer stays, as feeds give only the newest, up to 200 for each channel.
An unfollowed channel's videos go from the feed at once and from the file at
the next update's end.

**When.** On opening the section or coming back to it, if the last check was
over 30 minutes ago; by hand at any time. After YouTube refused, opening it
leaves YouTube alone for an hour, while Update Now still asks. Only an update
that reaches its end counts as a check. One stopped by leaving the section,
quitting or a crash doesn't, and the next open asks only the channels it hadn't
reached, as they are the ones not answered in the last 30 minutes. Coming back
while it winds down is taken up as it ends. A channel never asked, because its
own fetch was stopped, is asked on opening though no update is due. Saved
videos show at once, answers change the lines in place at most every half a
second, and `youtube-feed.json` and any changed channel names are written on
another thread every 30 seconds and at the end, and once more when quitting.
The feed can always be fetched again, so an unreadable one is simply started
over.

**Shorts.** Only a video known to be a Short is hidden, in the feed and in
search, each with a switch to show them. Watch Later keeps what was put on it,
and a channel's page lists its uploads, which leave them out. Knowing takes a
channel feed or a `/shorts/` address, so turning the switch on during an outage
shows few of them.

**Live.** A badge shows while it can be believed: 12 hours since a live
stream was last seen so, 30 days for an upcoming one, as feeds don't say
when a stream ends. A length clears it. A live stream can't be downloaded
until it has ended.

## Search

On Enter, one process asks for `ytsearch30:<text>` and for the results page
filtered to channels, a few seconds together, and either can fail alone. The
page shows at most 5 channels above the videos, as most are near misses. The
text is cut to 200 characters. A new search cancels the last, and so do
emptying the field and Back from the page, which stop yt-dlp: what was
searched for would otherwise arrive unasked or unseen. Text typed over the
query, or another page of the bar, leaves the search coming, as the results
are still wanted until Enter asks for others. Results aren't saved, and
thumbnails aside nothing of them is kept.

What search finds is whatever anyone has uploaded. Patina adds no filter and
asks for no Restricted Mode. What YouTube keeps for signed-in adults takes
an account, and fails here with that said.

## Channels

What is pasted is read for its parts (`ChannelInput`), never used as it is:
an id, an `@handle`, or a link on `youtube.com` to `/channel/`, `/@`, `/c/`
or `/user/`. The address yt-dlp gets is built from those. It answers with
the channel's id, name and picture, without listing any videos
(`--playlist-items 0`). A channel's picture is an address from yt-dlp, kept
only if it is HTTPS on a host in `client.rs`'s list. The channel feeds say
nothing of a picture, so a channel imported from a file has none until yt-dlp
has been asked about it: by a listing, as when the feed fails for it, or by
opening its page, whose answer a followed channel keeps.

A channel's page shows what is saved at once, then up to 30 videos from
yt-dlp. If those don't come, what stopped them shows above the saved ones,
with Try Again.

## Thumbnails

A card asks for `mqdefault.jpg`, which exists for every video. Past 400
physical pixels wide it also asks for `hq720.jpg`, with a 404 falling back
to `mqdefault` and then `hqdefault`, whose black bars are at least a
picture. YouTube sends an image with a 404 as well, so only a 2xx is kept.
They go through the same cache as Jellyfin's posters (`jellyfin/images.rs`)
with a pool of their own, in memory only near the screen and on disk in
`youtube-images`, limited to 128 MB, the least recently used going first. A
cached picture isn't asked for again. A changed proxy starts the pool afresh
so that no picture goes by the old way.

`client.rs` allows `https://` on the default port to its listed hosts only,
with no user name in the address, and follows redirects itself, so that
every hop is checked. It waits 5 seconds to connect and 10 in all, follows 3
redirects, and reads at most 2 MB of feed or 8 MB of picture.

## Playback

A click on a card plays that video. Play From Here plays the page's list
from there, and Play All does the same on Watch Later, with next and
previous going through it. A list gives mpv at most 200 entries, 50 of them
before the one that plays. A video goes to mpv by its one address
(`VideoId::watch_url`), since history and progress match on the exact text,
or as its own file when it was downloaded.

The window plays through mpv's hook, as Open URL does, with a mark rather
than a check of the address (`from_section`, set by `Placement::Section`),
so the same address from Open URL behaves as it always did. For a file with
that mark, the load hook sets, as file-local options that end with it:

- its title, so that it shows from the start rather than an address;
- where it was left (see Watched marks), when the Resume preference is on;
- the section's yt-dlp options (`Preferences::ytdl_options_for_section`):
  `ignore-config`, the client argument and the proxy, nothing of the user's,
  through the setter that keeps it out of mpv's log;
- a format up to the height of the screen the window is on when the video
  loads.

Any YouTube address, the section's or not, is also streamed in 10 MiB parts
(architecture.md's Online video says why). Measured on Linux with a 4K video
of 9.4 Mb/s on a fast connection: mpv's single request came at 2.3 MB/s, the
cache grew by a third of a second each second and held 8 seconds after a seek;
in parts it came at over 15 MB/s, and the cache reached its 150 MiB limit,
over two minutes of that video, within 8 seconds. yt-dlp's clients made no
difference to that, nor did decoding, which kept far ahead in software alone.
Not every stream is held back: of others tried, the AV1 ones were, while a
1080p VP9 one came at full speed in a single request, and a little slower,
though still far ahead of playback, in parts.

**Quality.** `youtube-quality` in Preferences › Network chooses Up to the
screen's resolution (the default), Best available, or a height. The screen's
short side is the limit, as a screen on its side still shows a wide video
across its long one, and 1080 is used if the screen can't be read. A
`ytdl-format` preference wins over it. The Video Quality menu works as usual
and keeps that limit.

A card's menu has Copy Link, which copies the video's address built again from
its id (`VideoId::watch_url`), never one an answer gave.

**Ending.** Finishing, Escape, Stop and seeking past the end all return the
window to the section at the same page and scroll. A list goes on to its
next entry first. A window waiting for yt-dlp says "Waiting for YouTube…"
with a Stop button. The video clicked enters Recent as it opens, as Open URL's
does, and the rest of a list only when they play.

**Failing.** mpv's hook logs what yt-dlp said, which is the only place it is
said, so the window hears it (`youtube::ytdlp::hook_failure`) and the
section names it in a message under the bar, which stays until dismissed; with
no section showing, as from Recent, it says so in the window. A problem with
one video goes on to the next in a list; a block or a network failure stops the
list, as each would fail the same way.

## Watched marks

A video is marked by how much was really seen, not by being opened.
`Sitting` counts how far the position moved by playing: not seeks, not
pauses, and a jump of over 5 seconds is taken for a seek. A mark becomes
"watched" at 80% of the video's length, which leaves room for an end card.
Cards show a progress strip. A video not yet watched picks up where it was
left if that is at least 10 seconds in and more than 15 from the end.

Any video opened by its one address gets a mark, whichever way it was
opened, or as a file the section downloaded. With "Remember playback
history" off nothing is noted, though a video can still be marked by hand.
Clear Recent, the History page's Clear History, and every other way of
clearing the history, clears the marks, and removing one history entry clears
that video's. Marks are only ever kept here.

## History

The History page is the player's history, the one Recent and the History
window show, with only its YouTube videos: those opened by their one address,
whichever way, and the files the section downloaded, each once, newest first,
with when it last played. It keeps nothing of its own. The app hands the
section its history whenever that changes (`Session::set_history`), a file's
length and position at its end included, and which entries are videos is
worked out only when the page shows, as telling a downloaded file needs
`youtube.json`, which nothing reads before the section does. What the lists
know of a video, its title, channel and length, fills in what the history
doesn't keep; the history's title and length are used otherwise, from an older
entry of the same video where the newest has none: a title that is only the
address or a file's name is none, and a file that has only just loaded has no
length. A downloaded file with no title anywhere is called what its name says,
yt-dlp's "<title> [<id>]". With "Remember playback history" off, nothing is
added, and the empty page says so.

Clear History asks first, in a dialog, as the clearing goes further than the
page: the YouTube videos leave the history, Recent and the History window
with them, other files stay, and every watched mark and progress goes, marks
of videos not in the history included, as they say what was watched as much
as the history does.

## Downloads

Downloads go through the Downloads window's queue into the Preferences
download folder, sharing it with Jellyfin's and with File › Download Video,
whose rules don't change. A video from the section starts only while fewer
than 2 of yt-dlp are running and at least 8 seconds after the last started.
Jellyfin's downloads keep their one at a time from each server, and neither
kind holds the other up. A second request for a video queued or running is
ignored. There is no batch.

A download goes with the same section options as playing, none of the
user's, and the format of the quality preference. File › Download Video, for
a video the section opened, goes the same way in the quality playing.
Joining video and audio takes ffmpeg: without it, it is the best that comes
as one file. A failure is named in its row. A finished one is noted on its
video, whose card shows it, and from then on it plays from its file.

"Move Download to Trash" on the card sends the file to the trash; a file
gone already just loses its mark, and one that can't be trashed stays
marked, with the reason said. The Remove button in the Downloads window
still removes only the row, for every kind of download.

## yt-dlp

Each call is one short-lived process, found as the hook and downloads find
it, with time limits (30 seconds for search and details, 20 for adding a
channel, 20 plus 4 per channel for a listing), `--socket-timeout 10`, one
retry, a limit on its output, and `--` before anything from outside.
`--no-cache-dir` keeps these calls from leaving anything behind.

The section shows yt-dlp's version and age, from the version itself, which
needs no network, and looks again on coming back from a video and when the
preferences name another. Past 90 days the message under the bar says it looks
out of date, since YouTube changes often and fixes come in new releases. Patina
doesn't update it: that would put code outside the signed release into a
process with the network and the user's files.

With no yt-dlp, the section says so plainly and still shows the saved lists.
Every package carries one. The Flatpak carries yt-dlp, Deno and yt-dlp-ejs,
and uses the runtime's ffmpeg. Patina.app and the Windows folder carry
yt-dlp's own folder build (`Contents/Helpers/yt-dlp.app` in Patina.app, a
`yt-dlp` folder beside `patina.exe` on Windows) and an ffmpeg built from
FFmpeg's source for joining video and audio alone (`packaging/ffmpeg.sh`),
pinned in `packaging/bundled-tools.env` and found by `platform::bundled`, but
no JavaScript runtime: yt-dlp then asks YouTube only for its `visionos`
client, which needs none and which the section asks for first anyway. Tried
on Linux with `--no-js-runtimes`, on two videos, that gave the same best
format with no warning. On a macOS and a Windows VM the packages' yt-dlp,
with no runtime, played and downloaded public videos, up to 1080p on macOS
and 4K on Windows; that is all that has been checked. The order is the
yt-dlp the user chose (the path preference, a `ytdl_path` in mpv's
`script-opts/ytdl_hook.conf`, or a `yt-dlp` in mpv's configuration folder),
then the newer of the bundled one and a `yt-dlp` the user installed (on the
`PATH`, and on macOS in Homebrew's folders too), then the `PATH`; a
`youtube-dl` doesn't count anywhere there, as the section can't use it.
Each copy is asked its version once, off the interface thread, and the
answer is kept until the file changes (`ytdl_versions.rs`); until both have
answered, the bundled one is used. The section's age notice is of the one in
use. A Linux build looks exactly as before. Downloads get
the bundled ffmpeg with `--ffmpeg-location`, which wins over an
`ffmpeg-location` in the user's own yt-dlp configuration file (the section's
own downloads read none); naming one in Patina's yt-dlp options wins over
the bundled one. The bundled yt-dlp is GPL-3.0-or-later as a whole, for the
libraries its build carries; `packaging/THIRD-PARTY.md` says what and where
their source is. The bundled copies
are only as new as the release, which is why the pins are moved to yt-dlp's
newest release for every release (the comment in that file says how), and
why the age above matters most there.

## Failures by name

yt-dlp's errors are English text that changes between versions, so reading
them is a matter of finding known phrases (`classify`), with a plain message
when none is there. The interface names: YouTube blocking the address, the
network, the proxy, the feed server, yt-dlp missing, out of date or off, a
time limit; for a channel: gone or not available; and for a video: private,
for signed-in adults, not in this country, for members, removed, not started
yet, or otherwise unavailable.
These phrases are fixtures (`fixtures/video-errors.txt`) from research and
yt-dlp's source. Only "not available" and "does not exist" have been seen
live.

## Proxy

There is one rule for what talks to YouTube. `Preferences::proxy` is a
`proxy` in the yt-dlp options if there is one (empty meaning direct), else
the HTTP proxy preference, and with neither, the system's proxy settings
apply as they do for any program. It goes to feed and picture requests and
to every yt-dlp call, and through mpv's hook and `http-proxy` to its
streams. A proxy that can't be used is an error, for feeds and pictures
alike, and nothing goes direct instead. Outside the section the HTTP proxy
preference reaches yt-dlp too, from mpv's hook and from downloads, unless
the yt-dlp options name their own. OpenSubtitles and Jellyfin requests don't
read this preference.

## Untrusted input

A feed, a yt-dlp answer and a chosen file are all treated as someone else's.
Ids are checked when they arrive, every address is built from a checked id,
others' text is cleaned before it is shown or kept, and an id or search
can't become an option, as `--` comes before them.

## Interface

The welcome screen's left column has "YouTube" after Open and Open URL,
above the Jellyfin servers, and the File menu has it beside Jellyfin…. The
window action `youtube` opens the section in an idle window, or in a new one
while something plays. The section is a third view in the place of the
welcome view. `ui/app.slint` shows exactly one of them (`youtube-open`,
`library-open`). A file playing hides the section, and File › Jellyfin…
leaves it first, or sign-in would open behind it.

Slint keeps the focus on items that have gone, and hands every key to
nothing until Tab or a menu gives it back, so `changed youtube-open` gives
the focus to the player's keys as the section goes, and the view takes it
when it comes. For everything else that leaves it so (a card scrolled out of a
list or made again by a load, a choice from a submenu), `keep_keyboard` in
`window.rs` looks ahead of each of the window's events and has the view take the
keyboard back, which leaves a control that is still there its focus. A dialog's
closing asks the same: the field being typed in has it back, or else the view.
Nothing that arrives under a dialog takes the keyboard from it, and what is typed
while the Channels page's field waits on a channel being added stops at the view.

The bar's Back and the mouse's back button go through pages and then leave,
for the library if the section was opened over it. The title bar's Home
button leaves both for the welcome screen (the window action `welcome`). The
History page's Clear History… opens the window's dialog
(`clear-youtube-history`), whose closing gives the view the keyboard back as
any dialog's does.
Each page keeps its scroll when left. Lines are made afresh only when a page
is shown; what comes later changes them in place, so a list being read
doesn't jump. Pictures are held only near the screen, as a decoded image for
each of thousands of videos would run to gigabytes. The view says what of a
list is in sight, in pixels, with the height of each kind of line
(`YouTubeSight`), and the controller counts the lines from those: a channel's
line is a third as tall as a line of videos, and a count that took every line
for one of those left most of a long Channels page without pictures until the
window was made larger.

## Tests

The tests sit beside the code and need no network or real yt-dlp, and CI
doesn't reach YouTube. They use made-up saved answers (`fixtures/`), a small
HTTP server on 127.0.0.1 (`client::testing`), and on Unix a stand-in script
for yt-dlp (`ytdlp::testing`). Sessions take sources, pictures and a trash
of the test's own. They cover hostile ids, addresses, XML, CSV and JSON; the
breaker, batches and mapping by position; a 404 never marking a channel
gone; a proxy never going direct; the two Google files round-tripping byte
for byte; what a request carries
(`a_request_says_who_is_asking_and_nothing_of_the_user`); the section's
arguments to yt-dlp; the queue's limit and spacing; time seen; the lines in
sight and the pictures asked for them; the History page and its clearing; and
the pages, focus, Back, Copy Link and the Clear History dialog through the
Slint testing backend.

To try it by hand without YouTube, point the `ytdl-path` preference at a
stand-in with its own settings and data folders (development.md). A
stand-in's JSON must have `"proxy":""`, or mpv's hook applies the proxy to a
local stream.

## Not yet tried

Checked with real yt-dlp and feeds on Linux, on a virtual framebuffer, with
a handful of requests against public channels.

The macOS and Windows packages the release workflow built have been through
two rounds on virtual machines, with a few requests against public videos
and no sign-in: an Intel macOS 26 VM with no GPU, running Patina.app ad-hoc
signed, as the workflow leaves it; and a Windows 11 VM with no GPU and no
audio device, running the installer and the portable zip. On both,
yt-dlp's folder build starts in about a third of a second and unpacks
nothing; the single-file build the packages carried before took about 10
seconds to start on the macOS VM, and 1.3 to 1.5 on Windows. A first start
takes longer: about 2 seconds on the macOS VM, and once 6 seconds for a
portable copy just unpacked on Windows, for a reason not found. Search,
playback and downloads worked, the bundled ffmpeg joining each download's
video and audio (from 720p H.264 to 4K AV1), and stopping a search or a
download left no process and no unpacked `_MEI` folder behind. On Windows
that was done in the section itself: the yt-dlp version under the bar,
search, playing from the section and through Open URL, downloading, Cancel
in the Downloads window, Back during a search, quitting during a download,
and the installed and portable copies each using its own yt-dlp. The macOS
VM took no input, so there the section's commands were run as it builds
them, and a YouTube address given to Patina on the command line played
through the bundled yt-dlp.

Not yet tried:

- Apple Silicon (the universal app's arm64 half has never run), macOS 11,
  and the Flatpak.
- The Developer ID signed, notarised Patina.app: whether its hardened
  runtime lets yt-dlp.app load Python.framework and its modules, and a
  quarantined copy, as a browser or Homebrew leaves it.
- The section on macOS by hand: its version line, search, playing from it,
  downloads, and Cancel and Back from the app's own buttons.
- Killing Patina by force while a download runs (on Windows, Patina's own
  stopping already kills yt-dlp by force, and that left nothing behind);
  cancelling during ffmpeg's join, which the downloads tried finished too
  quickly to catch; and a search running out of time.
- On Windows, the bundled yt-dlp through an upgrade over an older install
  or a running download, and SmartScreen and Smart App Control
  (`packaging/windows/README.md`).
- Sound, and a GPU: both VMs rendered in software, and neither played audio.
- Choosing the newer of the bundled yt-dlp and one the user installed
  (Homebrew, winget, pip) on a real Mac or Windows machine: the comparison,
  the kept answers and a slow or broken copy were tried on Linux with
  stand-ins that print a version, and the hook, section and Downloads
  window moving to the installed copy once it has answered not at all.
- The player refusing to download a live stream with the bundled ffmpeg,
  which needs a live stream playing on macOS or Windows.
- The import and export dialogs themselves: the test display has no portal,
  and neither VM opened them. What they call is tested below them, and
  importing through the view.
- A real export's rarer forms, and a translated one, whose header fallback
  is a guess.
- NewPipe importing the file Patina writes, and Patina importing a file a
  real NewPipe wrote. Both follow NewPipe's own source (0.29.1) and its test
  file's form, but neither has been tried with the app.
- A real feed outage end to end, a real block, and real private,
  age-restricted, region-locked and members-only videos, whose wording comes
  from research.
- A real proxy with yt-dlp, and SOCKS through the feed and picture client.
- Reading the screen on Wayland, the quality limit having run on X only.
- A real video with an ad, a stream failing partway, a real `hq720` picture
  at scale 2.
- Streaming in parts for a live stream, whose playlist comes from another of
  YouTube's hosts, through a proxy, or with an FFmpeg too old to have
  `request_size`, which mpv then only logs as an option it couldn't set (tried
  with a made-up option on a local file). Measured on one public 4K video,
  against Patina built without it and mpv alone.
- Downloads started from File › Download Video and the Retry button, on
  screen, and downloading without ffmpeg, beyond a unit test.
