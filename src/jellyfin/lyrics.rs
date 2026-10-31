//! A song's lyrics as the playlist drawer's Lyrics tab shows them
//! (`GET /Audio/{id}/Lyrics`): the lines, each with when it begins when
//! the lyrics are timed, and which line is being sung.

use super::models::{LyricDto, ticks_to_seconds};

/// How long before its start a line counts as begun. After an exact seek
/// made while paused, mpv says the position is a little short of where it
/// went, by nearly the audio it keeps buffered for the output (a fifth of
/// a second unless changed), so a line clicked to play from would show the
/// line before it; and a line lit that moment early is no harder to
/// follow.
const LEAD: f64 = 0.25;

/// One line, and when it begins, in seconds, when the lyrics are timed.
/// An empty line is a break between verses: while it is the one reached,
/// no words are being sung.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub text: String,
    pub start: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    /// The lines say when they begin: the one being sung can be followed,
    /// and a line played from.
    pub timed: bool,
}

impl Lyrics {
    /// The lyrics as shown: no empty lines before the first words or
    /// after the last (a text file's last line break makes one), and one
    /// for each run of them between. None when no words are left.
    pub fn new(dto: &LyricDto) -> Option<Self> {
        let timed = dto.is_timed();
        let mut lines: Vec<Line> = Vec::with_capacity(dto.lyrics.len());
        for line in &dto.lyrics {
            let text = line.text.trim();
            let follows_break = lines.last().is_none_or(|last| last.text.is_empty());
            if text.is_empty() && follows_break {
                continue;
            }
            lines.push(Line {
                text: text.to_owned(),
                start: line.start.filter(|_| timed).map(ticks_to_seconds),
            });
        }
        while lines.last().is_some_and(|last| last.text.is_empty()) {
            lines.pop();
        }
        (!lines.is_empty()).then_some(Self { lines, timed })
    }

    /// Where clicking line `index` plays from: its start, for a line of
    /// words in timed lyrics. None for a break between verses, and for a
    /// line the server gave no start although it says the lyrics are
    /// synced, which a lyrics provider may do: the line is never the one
    /// sung, and a seek to the -1 that stands for it would count back from
    /// the file's end.
    pub fn plays_from(&self, index: usize) -> Option<f64> {
        let line = self.lines.get(index)?;
        line.start
            .filter(|start| self.timed && !line.text.is_empty() && *start >= 0.0)
    }

    /// The line being sung at `position` seconds: the one begun latest,
    /// counting from `LEAD` before each start. None before the first has
    /// begun, and for lyrics that aren't timed.
    pub fn line_at(&self, position: f64) -> Option<usize> {
        if !self.timed {
            return None;
        }
        let mut found: Option<(usize, f64)> = None;
        for (index, line) in self.lines.iter().enumerate() {
            let Some(start) = line.start else {
                continue;
            };
            // The server sorts them, but a later line of the same time
            // (or an earlier one listed after) counts as begun latest.
            if start <= position + LEAD && found.is_none_or(|(_, latest)| start >= latest) {
                found = Some((index, start));
            }
        }
        found.map(|(index, _)| index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jellyfin::models::{LyricLine, LyricMetadata};

    fn dto(lines: &[(&str, Option<f64>)]) -> LyricDto {
        LyricDto {
            metadata: LyricMetadata::default(),
            lyrics: lines
                .iter()
                .map(|(text, start)| LyricLine {
                    text: (*text).to_owned(),
                    start: start.map(|seconds| (seconds * 10_000_000.0) as i64),
                    cues: Vec::new(),
                })
                .collect(),
        }
    }

    fn texts(lyrics: &Lyrics) -> Vec<&str> {
        lyrics.lines.iter().map(|line| line.text.as_str()).collect()
    }

    #[test]
    fn the_line_sung_is_the_one_begun_latest() {
        let lyrics = Lyrics::new(&dto(&[
            ("The water leaves the harbour wall", Some(0.5)),
            ("and takes the evening light", Some(3.0)),
            ("", Some(6.0)),
            ("until the tide comes back tonight", Some(9.0)),
        ]))
        .unwrap();
        assert!(lyrics.timed);
        assert_eq!(lyrics.lines[1].start, Some(3.0));
        // Nothing yet before the first line, then each from a moment
        // before its start on.
        assert_eq!(lyrics.line_at(0.0), None);
        assert_eq!(lyrics.line_at(0.24), None);
        assert_eq!(lyrics.line_at(0.25), Some(0));
        assert_eq!(lyrics.line_at(2.74), Some(0));
        assert_eq!(lyrics.line_at(2.75), Some(1));
        // Where mpv says a paused seek to a line went: a sample short of
        // it, or short by most of its audio buffer.
        assert_eq!(lyrics.line_at(0.499977), Some(0));
        assert_eq!(lyrics.line_at(8.852415), Some(3));
        // A break between verses is reached like a line, with no words.
        assert_eq!(lyrics.line_at(7.0), Some(2));
        assert_eq!(lyrics.line_at(9.5), Some(3));
        assert_eq!(lyrics.line_at(600.0), Some(3));
    }

    #[test]
    fn lines_out_of_order_or_at_the_same_time_count_by_their_start() {
        let lyrics = Lyrics::new(&dto(&[
            ("chorus", Some(10.0)),
            ("first verse", Some(2.0)),
            ("echo", Some(10.0)),
            ("second verse", Some(20.0)),
        ]))
        .unwrap();
        assert_eq!(lyrics.line_at(5.0), Some(1));
        assert_eq!(lyrics.line_at(12.0), Some(2));
        assert_eq!(lyrics.line_at(25.0), Some(3));
    }

    #[test]
    fn lyrics_without_times_are_text_alone() {
        // As the server reads a .txt file: its last line break makes an
        // empty last line.
        let lyrics = Lyrics::new(&dto(&[
            ("Glass water, still and clear", None),
            ("Every stone is showing here", None),
            ("", None),
        ]))
        .unwrap();
        assert!(!lyrics.timed);
        assert_eq!(
            texts(&lyrics),
            [
                "Glass water, still and clear",
                "Every stone is showing here"
            ]
        );
        assert!(lyrics.lines.iter().all(|line| line.start.is_none()));
        assert_eq!(lyrics.line_at(5.0), None);
        // Some lines timed and others not: not timed, unless the server
        // says they are synced.
        let mixed = dto(&[("one", Some(1.0)), ("two", None)]);
        assert!(!Lyrics::new(&mixed).unwrap().timed);
        let mut synced = mixed.clone();
        synced.metadata.is_synced = Some(true);
        let synced = Lyrics::new(&synced).unwrap();
        assert!(synced.timed);
        assert_eq!(synced.line_at(5.0), Some(0));
        // Untimed lines play from nowhere.
        assert_eq!(lyrics.plays_from(0), None);
        assert_eq!(Lyrics::new(&mixed).unwrap().plays_from(0), None);
    }

    #[test]
    fn a_synced_line_without_a_start_plays_from_nowhere() {
        // As a lyrics provider may send them: synced, with a line the
        // server has no start for.
        let mut synced = dto(&[
            ("The first line", Some(1.0)),
            ("A line the server gave no start", None),
            ("", Some(3.0)),
            ("The last line", Some(4.0)),
        ]);
        synced.metadata.is_synced = Some(true);
        let lyrics = Lyrics::new(&synced).unwrap();
        assert!(lyrics.timed);
        assert_eq!(lyrics.plays_from(0), Some(1.0));
        assert_eq!(lyrics.plays_from(1), None);
        // A break between verses, and past the end.
        assert_eq!(lyrics.plays_from(2), None);
        assert_eq!(lyrics.plays_from(3), Some(4.0));
        assert_eq!(lyrics.plays_from(4), None);
        // It is never the line sung: the one before it stays lit.
        assert_eq!(lyrics.line_at(2.0), Some(0));
        assert_eq!(lyrics.line_at(4.0), Some(3));
    }

    #[test]
    fn empty_lines_go_from_the_ends_and_runs_of_them_become_one() {
        let lyrics = Lyrics::new(&dto(&[
            ("", Some(0.0)),
            ("  ", Some(0.2)),
            ("first ", Some(1.0)),
            ("", Some(2.0)),
            ("", Some(3.0)),
            (" second", Some(4.0)),
            ("", Some(5.0)),
            ("", Some(6.0)),
        ]))
        .unwrap();
        assert_eq!(texts(&lyrics), ["first", "", "second"]);
        // A break begins with the first empty line of its run.
        assert_eq!(lyrics.lines[1].start, Some(2.0));
        assert_eq!(lyrics.line_at(0.5), None);
        assert_eq!(lyrics.line_at(3.5), Some(1));
        // Nothing but breaks: nothing to show.
        assert_eq!(Lyrics::new(&dto(&[("", None), (" ", None)])), None);
        assert_eq!(Lyrics::new(&dto(&[])), None);
    }

    #[test]
    fn the_servers_lyrics_read_as_shown() {
        let timed: LyricDto = serde_json::from_str(include_str!("fixtures/lyrics.json")).unwrap();
        let timed = Lyrics::new(&timed).unwrap();
        assert!(timed.timed);
        assert_eq!(timed.lines.len(), 4);
        assert_eq!(timed.line_at(3.5), Some(1));
        let plain: LyricDto =
            serde_json::from_str(include_str!("fixtures/lyrics_unsynced.json")).unwrap();
        let plain = Lyrics::new(&plain).unwrap();
        assert!(!plain.timed);
        // The file's last line break is gone.
        assert_eq!(plain.lines.len(), 4);
        assert_eq!(plain.lines[3].text, "Only light on little things");
    }
}
