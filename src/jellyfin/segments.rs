//! Media segments: the intros, credits, recaps, previews and commercials a
//! server marks in an item (`GET /MediaSegments/{id}`, when a plugin found
//! them), and what a player window does while one plays: offer a Skip
//! button, skip it at once, or nothing, as the preferences say for each
//! kind.

use super::models::{MediaSegmentDto, SegmentKind};
use crate::settings::Preferences;

/// The least of a span still to play for it to be worth skipping: a span
/// shorter than this is dropped, and near its end the button goes, so that
/// a seek to the end (which may land a frame short) doesn't bring it back.
const LEAST_LEFT: f64 = 1.0;

/// What a window does while a kind of segment plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Shows a Skip button over the video.
    Ask,
    /// Skips to the span's end, saying so.
    Skip,
    Nothing,
}

impl Action {
    /// The preference for `kind`: "ask", "skip" or "none".
    pub fn of(prefs: &Preferences, kind: SegmentKind) -> Self {
        let value = match kind {
            SegmentKind::Intro => &prefs.jellyfin_skip_intro,
            SegmentKind::Outro => &prefs.jellyfin_skip_outro,
            SegmentKind::Recap => &prefs.jellyfin_skip_recap,
            SegmentKind::Preview => &prefs.jellyfin_skip_preview,
            SegmentKind::Commercial => &prefs.jellyfin_skip_commercial,
            SegmentKind::Unknown => return Action::Nothing,
        };
        match value.as_str() {
            "ask" => Action::Ask,
            "skip" => Action::Skip,
            _ => Action::Nothing,
        }
    }
}

/// The button's words for a kind of span.
pub fn button_label(kind: SegmentKind) -> &'static str {
    match kind {
        SegmentKind::Intro => "Skip Intro",
        SegmentKind::Outro => "Skip Credits",
        SegmentKind::Recap => "Skip Recap",
        SegmentKind::Preview => "Skip Preview",
        SegmentKind::Commercial => "Skip Ad",
        SegmentKind::Unknown => "Skip",
    }
}

/// What the OSD says after a span was skipped without asking.
pub fn skipped_label(kind: SegmentKind) -> &'static str {
    match kind {
        SegmentKind::Intro => "Skipped Intro",
        SegmentKind::Outro => "Skipped Credits",
        SegmentKind::Recap => "Skipped Recap",
        SegmentKind::Preview => "Skipped Preview",
        SegmentKind::Commercial => "Skipped Ad",
        SegmentKind::Unknown => "Skipped",
    }
}

/// A span of the file, in seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub kind: SegmentKind,
    pub start: f64,
    pub end: f64,
}

/// The same span of the same item, as two answers of the server give it.
fn same(a: &Span, b: &Span) -> bool {
    a.kind == b.kind && (a.start - b.start).abs() < 0.01 && (a.end - b.end).abs() < 0.01
}

/// What to do at a position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Now {
    /// No span worth skipping plays.
    Nothing,
    /// Show the Skip button for this span.
    Offer(Span),
    /// Skip this span now: seek to its end.
    Skip(Span),
}

/// One playback's spans, and which of them were skipped without asking.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Segments {
    spans: Vec<Span>,
    /// By span: skipped without asking already. Played into again (a seek
    /// back), such a span is offered instead: the user went back for it.
    skipped: Vec<bool>,
}

impl Segments {
    /// The server's segments, in order, leaving out kinds Patina doesn't
    /// know and spans too short to skip. `duration` (when known) cuts a
    /// span off at the file's end: ticks rounded on the server can put it
    /// a little past.
    pub fn new(segments: &[MediaSegmentDto], duration: Option<f64>) -> Self {
        let mut spans: Vec<Span> = segments
            .iter()
            .filter(|segment| segment.kind != SegmentKind::Unknown)
            .map(|segment| Span {
                kind: segment.kind,
                start: segment.start().max(0.0),
                end: duration
                    .filter(|duration| *duration > 0.0)
                    .map_or(segment.end(), |duration| segment.end().min(duration)),
            })
            .filter(|span| span.end - span.start >= LEAST_LEFT)
            .collect();
        spans.sort_by(|a, b| a.start.total_cmp(&b.start).then(b.end.total_cmp(&a.end)));
        Self {
            skipped: vec![false; spans.len()],
            spans,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Takes from the segments of the playback this one replaces (a reload
    /// for another quality, version or subtitle) which spans were skipped
    /// without asking: still one playback for the user, a span they went
    /// back into isn't skipped again, wherever the reload picks up.
    pub fn keep_skipped(&mut self, before: &Segments) {
        for (span, skipped) in self.spans.iter().zip(self.skipped.iter_mut()) {
            *skipped |= before
                .spans
                .iter()
                .zip(&before.skipped)
                .any(|(old, &was)| was && same(old, span));
        }
    }

    /// What to do at `position`, with `action` saying what each kind
    /// asks for. A span whose kind asks for nothing is passed over, so
    /// that it hides no other. Of spans that overlap, the one that began
    /// first counts; once it is skipped, the next one at its end does. A
    /// span is skipped without asking once per playback.
    pub fn at(&mut self, position: f64, action: impl Fn(SegmentKind) -> Action) -> Now {
        let found = self.spans.iter().enumerate().find(|(_, span)| {
            span.start <= position
                && position < span.end - LEAST_LEFT
                && action(span.kind) != Action::Nothing
        });
        let Some((index, span)) = found else {
            return Now::Nothing;
        };
        match action(span.kind) {
            Action::Skip if !self.skipped[index] => {
                self.skipped[index] = true;
                Now::Skip(*span)
            }
            _ => Now::Offer(*span),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jellyfin::models::seconds_to_ticks;

    fn segment(kind: SegmentKind, start: f64, end: f64) -> MediaSegmentDto {
        MediaSegmentDto {
            kind,
            start_ticks: seconds_to_ticks(start),
            end_ticks: seconds_to_ticks(end),
            ..Default::default()
        }
    }

    fn all(action: Action) -> impl Fn(SegmentKind) -> Action {
        move |_| action
    }

    #[test]
    fn a_span_is_offered_while_it_plays_and_not_near_its_end() {
        let mut segments = Segments::new(
            &[
                segment(SegmentKind::Outro, 1300.0, 1400.0),
                segment(SegmentKind::Intro, 5.0, 95.0),
            ],
            Some(1400.0),
        );
        let intro = Span {
            kind: SegmentKind::Intro,
            start: 5.0,
            end: 95.0,
        };
        assert_eq!(segments.at(0.0, all(Action::Ask)), Now::Nothing);
        assert_eq!(segments.at(4.99, all(Action::Ask)), Now::Nothing);
        assert_eq!(segments.at(5.0, all(Action::Ask)), Now::Offer(intro));
        assert_eq!(segments.at(60.0, all(Action::Ask)), Now::Offer(intro));
        // A seek to the end may land a frame short: nothing then.
        assert_eq!(segments.at(94.96, all(Action::Ask)), Now::Nothing);
        assert_eq!(segments.at(95.0, all(Action::Ask)), Now::Nothing);
        assert_eq!(segments.at(600.0, all(Action::Ask)), Now::Nothing);
        let Now::Offer(outro) = segments.at(1350.0, all(Action::Ask)) else {
            panic!("no outro");
        };
        assert_eq!(outro.kind, SegmentKind::Outro);
        assert_eq!(button_label(outro.kind), "Skip Credits");
        // Offering is asked again and again; it changes nothing.
        assert_eq!(segments.at(60.0, all(Action::Ask)), Now::Offer(intro));
    }

    #[test]
    fn a_reload_keeps_what_was_skipped() {
        let found = [
            segment(SegmentKind::Recap, 0.0, 4.0),
            segment(SegmentKind::Intro, 5.0, 11.0),
        ];
        let mut before = Segments::new(&found, None);
        let Now::Skip(intro) = before.at(5.5, all(Action::Skip)) else {
            panic!("the intro wasn't skipped");
        };
        // The user went back into it and stays there: offered, then
        // reloaded at 7 s for another quality.
        assert_eq!(before.at(7.0, all(Action::Skip)), Now::Offer(intro));
        let mut after = Segments::new(&found, Some(30.0));
        after.keep_skipped(&before);
        assert_eq!(after.at(7.0, all(Action::Skip)), Now::Offer(intro));
        // The recap wasn't skipped before, and still is once.
        assert!(matches!(after.at(1.0, all(Action::Skip)), Now::Skip(_)));
        // Without it, a reload skips the intro straight away.
        let mut fresh = Segments::new(&found, None);
        assert_eq!(fresh.at(7.0, all(Action::Skip)), Now::Skip(intro));
    }

    #[test]
    fn a_span_is_skipped_once_then_offered() {
        let mut segments = Segments::new(&[segment(SegmentKind::Intro, 5.0, 95.0)], None);
        let Now::Skip(intro) = segments.at(10.0, all(Action::Skip)) else {
            panic!("not skipped");
        };
        assert_eq!(intro.end, 95.0);
        assert_eq!(segments.at(95.0, all(Action::Skip)), Now::Nothing);
        // Played into again by a seek back: the user went back for it.
        assert_eq!(segments.at(20.0, all(Action::Skip)), Now::Offer(intro));
        // The next playback skips it again.
        let mut again = Segments::new(&[segment(SegmentKind::Intro, 5.0, 95.0)], None);
        assert_eq!(again.at(20.0, all(Action::Skip)), Now::Skip(intro));
    }

    #[test]
    fn each_kind_does_what_its_preference_says() {
        let prefs = Preferences::default();
        assert_eq!(Action::of(&prefs, SegmentKind::Intro), Action::Ask);
        assert_eq!(Action::of(&prefs, SegmentKind::Outro), Action::Ask);
        assert_eq!(Action::of(&prefs, SegmentKind::Recap), Action::Ask);
        assert_eq!(Action::of(&prefs, SegmentKind::Preview), Action::Ask);
        assert_eq!(Action::of(&prefs, SegmentKind::Commercial), Action::Nothing);
        assert_eq!(Action::of(&prefs, SegmentKind::Unknown), Action::Nothing);
        let prefs = Preferences {
            jellyfin_skip_recap: "skip".into(),
            jellyfin_skip_intro: "none".into(),
            ..Default::default()
        };
        assert_eq!(Action::of(&prefs, SegmentKind::Recap), Action::Skip);
        assert_eq!(Action::of(&prefs, SegmentKind::Intro), Action::Nothing);

        // A recap inside the intro: with intros left alone, the recap is
        // skipped; then nothing more, since the intro asks for nothing.
        let mut segments = Segments::new(
            &[
                segment(SegmentKind::Intro, 0.0, 90.0),
                segment(SegmentKind::Recap, 10.0, 40.0),
                segment(SegmentKind::Commercial, 200.0, 230.0),
            ],
            None,
        );
        let action = |kind| Action::of(&prefs, kind);
        assert_eq!(segments.at(5.0, action), Now::Nothing);
        let Now::Skip(recap) = segments.at(12.0, action) else {
            panic!("the recap wasn't skipped");
        };
        assert_eq!((recap.kind, recap.end), (SegmentKind::Recap, 40.0));
        assert_eq!(segments.at(40.0, action), Now::Nothing);
        // Commercials are left alone by default.
        assert_eq!(segments.at(210.0, action), Now::Nothing);
    }

    #[test]
    fn overlapping_spans_go_one_after_another() {
        let mut segments = Segments::new(
            &[
                segment(SegmentKind::Intro, 30.0, 120.0),
                segment(SegmentKind::Recap, 0.0, 45.0),
            ],
            None,
        );
        let Now::Offer(first) = segments.at(35.0, all(Action::Ask)) else {
            panic!("nothing offered");
        };
        assert_eq!(first.kind, SegmentKind::Recap);
        // At the recap's end the intro still plays.
        let Now::Offer(next) = segments.at(first.end, all(Action::Ask)) else {
            panic!("nothing offered at the recap's end");
        };
        assert_eq!(next.kind, SegmentKind::Intro);
    }

    #[test]
    fn spans_too_short_unknown_or_past_the_end_are_trimmed() {
        let segments = Segments::new(
            &[
                segment(SegmentKind::Intro, 10.0, 10.5),
                segment(SegmentKind::Unknown, 20.0, 60.0),
                segment(SegmentKind::Outro, 1380.0, 1400.4),
                segment(SegmentKind::Preview, 1399.5, 1420.0),
            ],
            Some(1400.0),
        );
        assert_eq!(
            segments.spans,
            [Span {
                kind: SegmentKind::Outro,
                start: 1380.0,
                end: 1400.0,
            }]
        );
        assert!(Segments::new(&[], None).is_empty());
    }
}
