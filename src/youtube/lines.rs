//! A page's lines, as two models over one list: all of them, which says how
//! long the list is, and those near the sight, which are all the view makes.
//! The view places each line from the lines above it (`YouTubeLine::above`),
//! so the part it makes can move without the rest.

use crate::{YouTubeAbove, YouTubeLine};
use slint::{Model, ModelNotify, ModelRc, ModelTracker};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

#[derive(Default)]
pub struct PageLines {
    lines: RefCell<Vec<YouTubeLine>>,
    /// The lines asked to be drawn; those of them that exist are.
    wanted: Cell<(usize, usize)>,
    all: ModelNotify,
    near: ModelNotify,
}

/// The lines near the sight.
struct Near(Rc<PageLines>);

impl PageLines {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// For the view's `lines`.
    pub fn all(self: &Rc<Self>) -> ModelRc<YouTubeLine> {
        ModelRc::from(self.clone())
    }

    /// For the view's `near-lines`.
    pub fn near(self: &Rc<Self>) -> ModelRc<YouTubeLine> {
        ModelRc::new(Near(self.clone()))
    }

    /// The lines drawn, of all.
    pub fn drawn(&self) -> Range<usize> {
        let (start, end) = self.wanted.get();
        let length = self.lines.borrow().len();
        start.min(length)..end.min(length)
    }

    /// Draws these lines, and no others. The lines drawn before and still
    /// wanted stay as they are, so that a card under the pointer isn't made
    /// anew.
    pub fn draw(&self, wanted: Range<usize>) {
        let old = self.drawn();
        self.wanted
            .set((wanted.start, wanted.end.max(wanted.start)));
        let new = self.drawn();
        if old == new {
            return;
        }
        if new.start >= old.end || new.end <= old.start || old.is_empty() || new.is_empty() {
            self.near.row_removed(0, old.len());
            self.near.row_added(0, new.len());
            return;
        }
        // At the top, then at the bottom.
        if new.start > old.start {
            self.near.row_removed(0, new.start - old.start);
        } else if new.start < old.start {
            self.near.row_added(0, old.start - new.start);
        }
        if new.end < old.end {
            self.near
                .row_removed(new.end - new.start, old.end - new.end);
        } else if new.end > old.end {
            self.near.row_added(old.end - new.start, new.end - old.end);
        }
    }

    /// Draws from a screen above the first and last lines in sight to a
    /// screen below, once they come within half a screen of either end of
    /// what is drawn: half a screen's lines are then made or let go at a
    /// time, not a line with each step of the wheel. What is drawn stays
    /// within `images`, the lines that have their pictures, so that no line
    /// shows without them.
    pub fn draw_around(&self, first: usize, last: usize, images: Range<usize>) {
        let length = self.row_count();
        if length == 0 || first > last {
            return;
        }
        let screen = last - first + 1;
        let drawn = self.drawn();
        let needed = first.saturating_sub(screen / 2)..(last + screen / 2 + 1).min(length);
        let covered = drawn.start <= needed.start && needed.end <= drawn.end;
        let pictured = images.start <= drawn.start && drawn.end <= images.end;
        if !covered || !pictured {
            let start = first.saturating_sub(screen).max(images.start);
            let end = (last + screen + 1).min(images.end);
            self.draw(start..end);
        }
    }

    pub fn set_vec(&self, lines: Vec<YouTubeLine>) {
        *self.lines.borrow_mut() = lines;
        self.all.reset();
        self.near.reset();
    }

    pub fn push(&self, line: YouTubeLine) {
        let before = self.drawn();
        let index = {
            let mut lines = self.lines.borrow_mut();
            lines.push(line);
            lines.len() - 1
        };
        self.all.row_added(index, 1);
        // Drawn only if the lines asked for reach it, not just past the end
        // of a list that was shorter than where they start.
        if self.drawn().len() > before.len() {
            self.near.row_added(before.len(), 1);
        }
    }

    pub fn remove(&self, index: usize) {
        let before = self.drawn();
        if index >= self.lines.borrow().len() {
            return;
        }
        self.lines.borrow_mut().remove(index);
        self.all.row_removed(index, 1);
        if index < before.start {
            // The drawn lines are others now.
            self.near.reset();
        } else if index < before.end {
            self.near.row_removed(index - before.start, 1);
            if self.drawn().end == before.end {
                self.near.row_added(before.len() - 1, 1);
            }
        }
    }
}

impl Model for PageLines {
    type Data = YouTubeLine;

    fn row_count(&self) -> usize {
        self.lines.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<YouTubeLine> {
        self.lines.borrow().get(row).cloned()
    }

    fn set_row_data(&self, row: usize, data: YouTubeLine) {
        if let Some(line) = self.lines.borrow_mut().get_mut(row) {
            *line = data;
        } else {
            return;
        }
        self.all.row_changed(row);
        let drawn = self.drawn();
        if drawn.contains(&row) {
            self.near.row_changed(row - drawn.start);
        }
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.all
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Model for Near {
    type Data = YouTubeLine;

    fn row_count(&self) -> usize {
        self.0.drawn().len()
    }

    fn row_data(&self, row: usize) -> Option<YouTubeLine> {
        let drawn = self.0.drawn();
        if row < drawn.len() {
            self.0.row_data(drawn.start + row)
        } else {
            None
        }
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.0.near
    }
}

/// Says above each line how many of each kind there are, from which the view
/// places it.
pub fn place_lines(lines: &mut [YouTubeLine]) {
    let mut above = YouTubeAbove::default();
    for line in lines {
        line.above = above.clone();
        match line.kind.as_str() {
            "heading" => above.headings += 1,
            "channel" => above.channels += 1,
            _ => above.videos += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::SharedString;

    fn line(id: usize) -> YouTubeLine {
        YouTubeLine {
            id: SharedString::from(id.to_string()),
            kind: "videos".into(),
            ..Default::default()
        }
    }

    fn ids(model: &ModelRc<YouTubeLine>) -> Vec<String> {
        model.iter().map(|line| line.id.to_string()).collect()
    }

    /// What a model's tracker was told, in order.
    #[derive(Default)]
    struct Told(RefCell<Vec<String>>);

    impl i_slint_core::model::ModelChangeListener for Told {
        fn row_changed(self: std::pin::Pin<&Self>, row: usize) {
            self.0.borrow_mut().push(format!("changed {row}"));
        }
        fn row_added(self: std::pin::Pin<&Self>, index: usize, count: usize) {
            self.0.borrow_mut().push(format!("added {index}+{count}"));
        }
        fn row_removed(self: std::pin::Pin<&Self>, index: usize, count: usize) {
            self.0.borrow_mut().push(format!("removed {index}+{count}"));
        }
        fn reset(self: std::pin::Pin<&Self>) {
            self.0.borrow_mut().push("reset".to_owned());
        }
    }

    /// Listens to a model's changes, as a `for` in the view does.
    fn listen(
        model: &ModelRc<YouTubeLine>,
    ) -> std::pin::Pin<Box<i_slint_core::model::ModelChangeListenerContainer<Told>>> {
        let told = Box::pin(i_slint_core::model::ModelChangeListenerContainer::new(
            Told::default(),
        ));
        model
            .model_tracker()
            .attach_peer(told.as_ref().model_peer());
        told
    }

    fn take(
        told: &std::pin::Pin<Box<i_slint_core::model::ModelChangeListenerContainer<Told>>>,
    ) -> Vec<String> {
        std::mem::take(&mut *told.as_ref().get().0.borrow_mut())
    }

    #[test]
    fn the_near_lines_follow_what_is_drawn() {
        let lines = PageLines::new();
        lines.set_vec((0..10).map(line).collect());
        let near = lines.near();
        assert!(ids(&near).is_empty());
        lines.draw(2..5);
        assert_eq!(ids(&near), ["2", "3", "4"]);
        lines.draw(3..7);
        assert_eq!(ids(&near), ["3", "4", "5", "6"]);
        lines.draw(1..4);
        assert_eq!(ids(&near), ["1", "2", "3"]);
        lines.draw(8..20);
        assert_eq!(ids(&near), ["8", "9"]);
        lines.push(line(10));
        assert_eq!(ids(&near), ["8", "9", "10"]);
        lines.remove(9);
        assert_eq!(ids(&near), ["8", "10"]);
    }

    /// The lines drawn never reach past those with pictures, however the
    /// sight and the pictures' reach fall, and drawing the same again
    /// changes nothing.
    #[test]
    fn what_is_drawn_stays_within_the_pictures() {
        let lines = PageLines::new();
        lines.set_vec((0..200).map(line).collect());
        let near = lines.near();
        let told = listen(&near);
        for first in (0..195usize).step_by(7) {
            for screen in [1, 2, 3, 5] {
                let last = (first + screen - 1).min(199);
                for margin in [0, 1, screen, 2 * screen] {
                    let images = first.saturating_sub(margin)..last + margin + 1;
                    lines.draw_around(first, last, images.clone());
                    let drawn = lines.drawn();
                    assert!(
                        images.start <= drawn.start && drawn.end <= images.end,
                        "{first}..={last} with pictures {images:?}: drawn {drawn:?}"
                    );
                    assert!(
                        drawn.start <= first && last < drawn.end,
                        "{first}..={last}: drawn {drawn:?}"
                    );
                    take(&told);
                    lines.draw_around(first, last, images);
                    assert_eq!(lines.drawn(), drawn, "drawn again");
                    assert!(take(&told).is_empty(), "drawing the same again told");
                }
            }
        }
    }

    /// Half a screen at a time is drawn, not a line with each step, and a
    /// jump draws another part altogether.
    #[test]
    fn small_steps_keep_what_is_drawn_and_a_jump_replaces_it() {
        let lines = PageLines::new();
        lines.set_vec((0..100).map(line).collect());
        let near = lines.near();
        let told = listen(&near);
        let images = |first: usize, last: usize| {
            let margin = (last - first + 1) * 2;
            first.saturating_sub(margin)..last + margin + 1
        };
        lines.draw_around(20, 23, images(20, 23));
        assert_eq!(lines.drawn(), 16..28);
        take(&told);
        // Within half a screen of either end of it: nothing changes.
        for (first, last) in [(18, 21), (21, 24), (22, 25), (20, 23)] {
            lines.draw_around(first, last, images(first, last));
            assert_eq!(lines.drawn(), 16..28, "{first}..={last}");
        }
        assert!(take(&told).is_empty());
        // Nearer: drawn again around it, keeping the lines it shares.
        lines.draw_around(23, 26, images(23, 26));
        assert_eq!(lines.drawn(), 19..31);
        assert_eq!(take(&told), ["removed 0+3", "added 9+3"]);
        assert_eq!(ids(&near).first().map(String::as_str), Some("19"));
        // Far away: another part altogether.
        lines.draw_around(70, 73, images(70, 73));
        assert_eq!(lines.drawn(), 66..78);
        assert_eq!(take(&told), ["removed 0+12", "added 0+12"]);
        assert_eq!(ids(&near).first().map(String::as_str), Some("66"));
        // Nothing to draw, and a sight that makes no sense: left be.
        lines.draw_around(5, 2, 0..10);
        assert_eq!(lines.drawn(), 66..78);
        let empty = PageLines::new();
        let none = empty.near();
        let told_none = listen(&none);
        empty.draw_around(0, 0, 0..5);
        assert!(take(&told_none).is_empty());
        assert_eq!(none.row_count(), 0);
    }

    /// A line changed far from the sight changes nothing drawn.
    #[test]
    fn only_drawn_lines_tell_the_near_lines_of_a_change() {
        let lines = PageLines::new();
        lines.set_vec((0..30).map(line).collect());
        let near = lines.near();
        let all = lines.all();
        let told = listen(&near);
        let told_all = listen(&all);
        lines.draw(10..15);
        take(&told);
        lines.set_row_data(3, line(103));
        lines.set_row_data(12, line(112));
        lines.set_row_data(20, line(120));
        assert_eq!(take(&told), ["changed 2"]);
        assert_eq!(take(&told_all), ["changed 3", "changed 12", "changed 20"]);
        assert_eq!(ids(&near), ["10", "11", "112", "13", "14"]);
        // Past the end: nothing.
        lines.set_row_data(30, line(130));
        assert!(take(&told).is_empty() && take(&told_all).is_empty());
    }

    /// A line taken out above what is drawn, in it, or below it.
    #[test]
    fn a_line_taken_out_moves_what_is_drawn_as_it_should() {
        let lines = PageLines::new();
        lines.set_vec((0..20).map(line).collect());
        let near = lines.near();
        let told = listen(&near);
        lines.draw(5..9);
        take(&told);
        // Below: nothing drawn changes.
        lines.remove(15);
        assert!(take(&told).is_empty());
        assert_eq!(ids(&near), ["5", "6", "7", "8"]);
        // In it: the line after it moves up into it.
        lines.remove(6);
        assert_eq!(take(&told), ["removed 1+1", "added 3+1"]);
        assert_eq!(ids(&near), ["5", "7", "8", "9"]);
        // Above: the lines drawn are others.
        lines.remove(0);
        assert_eq!(take(&told), ["reset"]);
        assert_eq!(ids(&near), ["7", "8", "9", "10"]);
        // Past the end: nothing.
        lines.remove(100);
        assert!(take(&told).is_empty());
        // In a part that reaches the end: it gets shorter.
        lines.draw(13..30);
        take(&told);
        assert_eq!(ids(&near), ["16", "17", "18", "19"]);
        lines.remove(14);
        assert_eq!(take(&told), ["removed 1+1"]);
        assert_eq!(ids(&near), ["16", "18", "19"]);
    }

    /// Lines added to a list whose drawn part reaches past its end are
    /// drawn, and others aren't.
    #[test]
    fn lines_added_at_the_end_are_drawn_if_it_is() {
        let lines = PageLines::new();
        lines.set_vec((0..5).map(line).collect());
        let near = lines.near();
        let told = listen(&near);
        lines.draw(2..8);
        take(&told);
        lines.push(line(5));
        lines.push(line(6));
        assert_eq!(take(&told), ["added 3+1", "added 4+1"]);
        assert_eq!(ids(&near), ["2", "3", "4", "5", "6"]);
        lines.push(line(7));
        lines.push(line(8));
        assert_eq!(take(&told), ["added 5+1"]);
        assert_eq!(ids(&near), ["2", "3", "4", "5", "6", "7"]);
    }

    /// A page left empty keeps the lines asked for, far down, until the view
    /// next says what is in sight. Lines added to it are drawn only once
    /// they reach those, and the near lines are told no more than they have.
    /// Each was told as drawn before, with nothing drawn.
    #[test]
    fn lines_added_short_of_what_is_asked_for_are_not_drawn() {
        let lines = PageLines::new();
        lines.set_vec((0..300).map(line).collect());
        let near = lines.near();
        lines.draw_around(200, 202, 190..215);
        assert_eq!(lines.drawn(), 197..206);
        lines.set_vec(Vec::new());
        lines.draw_around(0, 0, 0..5);
        let told = listen(&near);
        for id in 0..199 {
            lines.push(line(id));
        }
        assert_eq!(take(&told), ["added 0+1", "added 1+1"]);
        assert_eq!(near.row_count(), 2);
        assert_eq!(ids(&near), ["197", "198"]);
    }

    /// Each line is told how many of each kind are above it, a line that
    /// changes kind included.
    #[test]
    fn each_line_counts_the_kinds_above_it() {
        let kinds = [
            "heading", "channel", "channel", "heading", "videos", "videos",
        ];
        let mut lines: Vec<YouTubeLine> = kinds
            .iter()
            .map(|kind| YouTubeLine {
                kind: (*kind).into(),
                ..Default::default()
            })
            .collect();
        place_lines(&mut lines);
        let above = |lines: &[YouTubeLine]| -> Vec<(i32, i32, i32)> {
            lines
                .iter()
                .map(|line| (line.above.headings, line.above.channels, line.above.videos))
                .collect()
        };
        assert_eq!(
            above(&lines),
            [
                (0, 0, 0),
                (1, 0, 0),
                (1, 1, 0),
                (1, 2, 0),
                (2, 2, 0),
                (2, 2, 1)
            ]
        );
        lines[2].kind = "videos".into();
        place_lines(&mut lines);
        assert_eq!(
            above(&lines),
            [
                (0, 0, 0),
                (1, 0, 0),
                (1, 1, 0),
                (1, 1, 1),
                (2, 1, 1),
                (2, 1, 2)
            ]
        );
    }
}
