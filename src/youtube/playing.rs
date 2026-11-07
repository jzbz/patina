//! What goes with playing a video from the section, apart from the window that
//! plays it: how much of it was really seen, where it picks up again, and which
//! part of a long list goes to the player.

use super::VideoId;
use super::store::Mark;

/// Time passing by more than this between two positions is taken for a jump
/// rather than for playing, in case mpv didn't say it was a seek.
const LONGEST_STEP: f64 = 5.0;
/// A video stopped before this has hardly been started, and starts over.
const RESUME_FROM: f64 = 10.0;
/// One stopped this close to its end has been seen, credits and all.
const RESUME_BEFORE_END: f64 = 15.0;
/// The entries a list gives the player. Each is a command to mpv, and a request
/// only once it plays, so this is a limit on the work of starting one, and on
/// the playlist someone has to scroll through.
const LIST_LIMIT: usize = 200;
/// How many of those go before the one that plays.
const LIST_BEFORE: usize = 50;
/// What the screen's size can't be read gets: a video this tall, at most.
const UNKNOWN_SCREEN: u32 = 1080;

/// One video's time in the player, from when it opens to when it ends. What
/// counts is how far the position moved by playing: not a seek, not a pause.
/// Watched marks go by this rather than by the video having been opened.
#[derive(Debug)]
pub struct Sitting {
    id: VideoId,
    /// The last position to count from. None after a seek, until mpv says where
    /// that landed.
    last: Option<f64>,
    position: f64,
    duration: f64,
    played: f64,
}

/// How a sitting went.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    pub id: VideoId,
    pub position: f64,
    pub duration: f64,
    pub played: f64,
}

impl Sitting {
    pub fn new(id: VideoId) -> Self {
        Self {
            id,
            last: None,
            position: 0.0,
            duration: 0.0,
            played: 0.0,
        }
    }

    /// mpv says where playback is. It only moves on while playing, so a paused
    /// video adds nothing, and at twice the speed its minutes still count as
    /// the video's.
    pub fn at(&mut self, position: f64) {
        if !position.is_finite() || position < 0.0 {
            return;
        }
        if let Some(last) = self.last {
            let step = position - last;
            if step > 0.0 && step <= LONGEST_STEP {
                self.played += step;
            }
        }
        self.last = Some(position);
        self.position = position;
    }

    pub fn lasts(&mut self, duration: f64) {
        if duration.is_finite() && duration > 0.0 {
            self.duration = duration;
        }
    }

    /// A seek began: wherever it lands isn't time played.
    pub fn seeked(&mut self) {
        self.last = None;
    }

    /// How it went, if anything happened: a file that opened and stopped at once
    /// leaves no mark.
    pub fn end(self) -> Option<Seen> {
        (self.played >= 1.0 || self.position >= 1.0).then_some(Seen {
            id: self.id,
            position: self.position,
            duration: self.duration,
            played: self.played,
        })
    }
}

/// Where a video opened from the section starts: where it was left, if that is
/// worth going back to.
pub fn resume_from(mark: &Mark) -> Option<f64> {
    let enough = mark.position >= RESUME_FROM;
    let before_end = mark.duration <= 0.0 || mark.position < mark.duration - RESUME_BEFORE_END;
    (!mark.watched && enough && before_end).then_some(mark.position)
}

/// The part of a list of `length` to give the player, for playing from `start`:
/// its range, and where `start` is in that.
pub fn list_part(length: usize, start: usize) -> (std::ops::Range<usize>, usize) {
    let start = start.min(length.saturating_sub(1));
    let from = start.saturating_sub(LIST_BEFORE);
    let to = (from + LIST_LIMIT).min(length);
    (from..to, start - from)
}

/// The tallest video the screen shows without scaling it down, by its short
/// side: a screen on its side still shows a 1920x1080 video across its 1920.
pub fn screen_height(screen: Option<(u32, u32)>) -> u32 {
    match screen {
        Some((width, height)) if width > 0 && height > 0 => width.min(height),
        _ => UNKNOWN_SCREEN,
    }
}

/// yt-dlp's filter for the section's quality preference, such as
/// `[height<=?1440]`. The `?` lets through a format that doesn't say. Empty for
/// the best there is.
pub fn video_filter(quality: &str, screen: Option<(u32, u32)>) -> String {
    let height = match quality {
        "best" => return String::new(),
        "screen" => screen_height(screen),
        other => other.parse().unwrap_or_else(|_| screen_height(screen)),
    };
    format!("[height<=?{height}]")
}

/// The format for playing with that filter. Always one that isn't empty: the
/// option mpv has otherwise is the Online video preference's, which isn't the
/// section's.
pub fn play_format(filter: &str) -> String {
    format!("bestvideo{filter}+bestaudio/best{filter}/best")
}

/// How much of a YouTube stream mpv asks for at a time, as FFmpeg's
/// `request_size`: 10 MiB, as yt-dlp's own downloads ask for (its
/// `http_chunk_size`). YouTube's servers sent some streams, each AV1 format
/// tried, at little more than the stream's own rate to a single open-ended
/// request: 2.3 MB/s for a 4K video of 9.4 Mb/s, where the same connection
/// fetched 10 MiB parts at over 15 MB/s, and at times 30. At the single
/// request's pace the cache grew by a third of a second each second, and after
/// a seek it held 8 seconds; anything slower for a moment and playback waited.
/// A stream sent at full speed to one request (a 1080p VP9 one was) comes a
/// little slower in parts, still far ahead of playback. mpv's yt-dlp hook
/// doesn't pass the part size yt-dlp gives for each format.
pub const REQUEST_SIZE: &str = "10485760";

/// Whether `url` is a YouTube page, which mpv's hook opens through yt-dlp and
/// then streams from YouTube's servers (`REQUEST_SIZE`), wherever it was opened
/// from. Other sites' servers may not answer a request for part of a file.
pub fn is_youtube(url: &str) -> bool {
    let url = url.strip_prefix("ytdl://").unwrap_or(url);
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| {
            host == "youtu.be" || host == "youtube.com" || host.ends_with(".youtube.com")
        })
}

/// The format for downloading with that filter. Joining video and audio takes
/// ffmpeg; without it, the best that comes as one file.
pub fn download_format(filter: &str, has_ffmpeg: bool) -> String {
    if has_ffmpeg {
        play_format(filter)
    } else {
        format!("best{filter}/best")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sitting() -> Sitting {
        Sitting::new(VideoId::parse("aaaaaaaaaaa").unwrap())
    }

    #[test]
    fn only_time_played_counts() {
        let mut sitting = sitting();
        sitting.lasts(100.0);
        // Where it opened isn't time played, resumed or not.
        sitting.at(40.0);
        sitting.at(40.5);
        sitting.at(41.0);
        // A pause says nothing new.
        sitting.at(41.0);
        // A seek ahead, however mpv says it.
        sitting.seeked();
        sitting.at(95.0);
        sitting.at(96.0);
        // A jump mpv didn't call a seek.
        sitting.at(10.0);
        sitting.at(90.0);
        sitting.at(91.0);
        let seen = sitting.end().unwrap();
        assert_eq!(seen.played, 3.0);
        assert_eq!(seen.position, 91.0);
        assert_eq!(seen.duration, 100.0);
    }

    #[test]
    fn a_seek_to_the_end_is_not_watching() {
        let mut sitting = sitting();
        sitting.lasts(600.0);
        sitting.at(0.0);
        sitting.at(1.0);
        sitting.seeked();
        sitting.at(599.0);
        let seen = sitting.end().unwrap();
        assert_eq!(seen.played, 1.0);
        assert_eq!(seen.position, 599.0);
    }

    #[test]
    fn nothing_seen_leaves_no_mark() {
        assert_eq!(sitting().end(), None);
        let mut barely = sitting();
        barely.at(0.2);
        barely.at(0.4);
        assert_eq!(barely.end(), None);
    }

    #[test]
    fn bad_numbers_change_nothing() {
        let mut sitting = sitting();
        sitting.lasts(f64::NAN);
        sitting.lasts(-3.0);
        sitting.at(2.0);
        sitting.at(f64::INFINITY);
        sitting.at(-1.0);
        sitting.at(3.0);
        let seen = sitting.end().unwrap();
        assert_eq!(seen.duration, 0.0);
        assert_eq!(seen.played, 1.0);
    }

    fn mark(position: f64, duration: f64, watched: bool) -> Mark {
        Mark {
            position,
            duration,
            seen: 0.0,
            watched,
            at: 0,
        }
    }

    #[test]
    fn a_video_picks_up_where_it_was_left_if_that_is_worth_it() {
        assert_eq!(resume_from(&mark(120.0, 600.0, false)), Some(120.0));
        assert_eq!(resume_from(&mark(120.0, 0.0, false)), Some(120.0));
        assert_eq!(resume_from(&mark(5.0, 600.0, false)), None);
        assert_eq!(resume_from(&mark(590.0, 600.0, false)), None);
        assert_eq!(resume_from(&mark(120.0, 600.0, true)), None);
    }

    #[test]
    fn a_long_list_goes_to_the_player_in_part() {
        assert_eq!(list_part(0, 0), (0..0, 0));
        assert_eq!(list_part(3, 1), (0..3, 1));
        assert_eq!(list_part(3, 9), (0..3, 2));
        assert_eq!(list_part(1000, 0), (0..200, 0));
        assert_eq!(list_part(1000, 30), (0..200, 30));
        assert_eq!(list_part(1000, 500), (450..650, 50));
        assert_eq!(list_part(1000, 999), (949..1000, 50));
    }

    #[test]
    fn the_default_quality_follows_the_screen_it_is_on() {
        for (screen, height) in [
            ((1366, 768), 768),
            ((1920, 1080), 1080),
            ((1920, 1200), 1200),
            ((2560, 1440), 1440),
            ((3440, 1440), 1440),
            ((3840, 2160), 2160),
            ((1080, 1920), 1080),
        ] {
            assert_eq!(
                video_filter("screen", Some(screen)),
                format!("[height<=?{height}]")
            );
        }
        assert_eq!(video_filter("screen", None), "[height<=?1080]");
        assert_eq!(video_filter("screen", Some((0, 0))), "[height<=?1080]");
        // A preference says otherwise.
        assert_eq!(video_filter("best", Some((1366, 768))), "");
        assert_eq!(video_filter("720", Some((3840, 2160))), "[height<=?720]");
        // A value a later version left behind.
        assert_eq!(video_filter("what", Some((1920, 1080))), "[height<=?1080]");
    }

    #[test]
    fn youtube_pages_are_streamed_in_parts() {
        for url in [
            "https://www.youtube.com/watch?v=aaaaaaaaaaa",
            "https://youtube.com/watch?v=aaaaaaaaaaa",
            "https://m.youtube.com/watch?v=aaaaaaaaaaa",
            "https://music.youtube.com/watch?v=aaaaaaaaaaa",
            "https://youtu.be/aaaaaaaaaaa",
            "http://www.youtube.com/playlist?list=PL",
            "ytdl://https://www.youtube.com/watch?v=aaaaaaaaaaa",
        ] {
            assert!(is_youtube(url), "{url}");
        }
        for url in [
            "https://example.com/watch?v=aaaaaaaaaaa",
            "https://notyoutube.com/watch?v=aaaaaaaaaaa",
            "https://youtube.com.example.com/watch",
            "/home/someone/youtube.com/a.mkv",
            "file:///youtube.com/a.mkv",
            "jellyfin://server/item",
            "",
        ] {
            assert!(!is_youtube(url), "{url}");
        }
    }

    #[test]
    fn formats_join_video_and_audio_only_if_they_can_be() {
        assert_eq!(
            play_format("[height<=?1080]"),
            "bestvideo[height<=?1080]+bestaudio/best[height<=?1080]/best"
        );
        assert_eq!(play_format(""), "bestvideo+bestaudio/best/best");
        assert_eq!(
            download_format("[height<=?720]", false),
            "best[height<=?720]/best"
        );
        assert_eq!(
            download_format("[height<=?720]", true),
            play_format("[height<=?720]")
        );
    }
}
