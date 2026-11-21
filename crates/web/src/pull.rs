//! Pull-down-to-search (spec §7.5, T26): the grid's gesture, and the sheet's answer to it.
//!
//! Pointer events only, and nothing bound here: [`Drag`] is the arithmetic, [`Pull`] is the
//! shared state, the feed owns the events and the sheet owns the pixels. The thresholds live in
//! one place so the browser spec can state the same number the gesture uses instead of drifting
//! from it by a few pixels of slop.
//!
//! The pull surface is the strip at the head of the grid rather than the whole feed, because a
//! browser hands a downward touch drag to its scroller before any pointer handler can object:
//! the pointer is cancelled the moment the compositor claims the pan. `touch-action: none` is
//! the one declaration that says "this surface is mine", and it can only be said on a surface
//! that never has to scroll — the strip, not the feed, and not the cards.

use dioxus::prelude::*;

/// The id of the pull surface. Also where the scroll check starts walking: whichever box ends
/// up holding the grid's offset, it is an ancestor of the strip.
pub const STRIP: &str = "pull";

/// Downward travel, in CSS pixels, at which releasing opens the sheet.
pub const OPEN_AT: f64 = 72.0;

/// The travel at which the panel has fully arrived. Past this the finger keeps going and the
/// panel stops following: a drag that stops answering is how a hand learns it is spent.
pub const TRAVEL: f64 = 160.0;

/// Movement under this is a tap. The grid is a list of links and every card stays pressable.
pub const SLOP: f64 = 10.0;

/// The gesture's state, provided once by the shell above the routes.
///
/// It lives above the grid because the sheet outlives the grid: a pull that gets half way and
/// then navigates must not leave the panel hanging in the tree, and the keyboard's door into
/// the same sheet has to clear whatever the last finger left behind.
#[derive(Clone, Copy)]
pub struct Pull {
    /// Downward travel of the live drag, in CSS pixels; `0` when nothing is being pulled.
    pub distance: Signal<f64>,
    /// A finger is on the strip and the panel is following it, so it must not animate.
    pub dragging: Signal<bool>,
    /// The panel is sliding away after a release that did not commit.
    pub closing: Signal<bool>,
}

impl Pull {
    /// Idle: nothing pulled, nothing open, nothing animating.
    pub fn new() -> Self {
        Self {
            distance: Signal::new(0.0),
            dragging: Signal::new(false),
            closing: Signal::new(false),
        }
    }

    /// The sheet is following the finger, `distance` pixels of the pull so far.
    pub fn follow(&mut self, distance: f64) {
        self.distance.set(distance);
        self.dragging.set(true);
        self.closing.set(false);
    }

    /// The finger let go past [`OPEN_AT`]: the panel finishes the trip on its own.
    pub fn settle_open(&mut self) {
        self.distance.set(0.0);
        self.dragging.set(false);
        self.closing.set(false);
    }

    /// The finger let go short of it: the panel slides from where it was left back out of view.
    pub fn settle_shut(&mut self) {
        self.distance.set(0.0);
        self.dragging.set(false);
        self.closing.set(true);
    }

    /// The slide is over — the panel may leave the tree.
    pub fn gone(&mut self) {
        self.closing.set(false);
    }

    /// Opened without a gesture (`/`, `Cmd-K`): in place, with nothing left over from a drag.
    pub fn show(&mut self) {
        self.distance.set(0.0);
        self.dragging.set(false);
        self.closing.set(false);
    }

    /// Anything of the panel still worth rendering: open, mid-pull, or sliding away.
    pub fn on_screen(&self, open: bool) -> bool {
        open || self.distance.cloned() > 0.0 || self.closing.cloned()
    }
}

/// How far the panel still sits below its resting place, as a fraction of its own height.
///
/// A fraction rather than pixels, so the drag does not have to measure the panel: the sheet
/// turns this into `translateY` and the browser does the arithmetic in its own units.
pub fn below(distance: f64) -> f64 {
    1.0 - (distance / TRAVEL).clamp(0.0, 1.0)
}

/// A release past [`OPEN_AT`] commits; anything shorter puts the panel back.
pub fn commits(distance: f64) -> bool {
    distance >= OPEN_AT
}

/// One finger's trip across the strip, in CSS pixels.
///
/// It exists because the question is not "did the finger move" but "did this drag start with
/// the grid at the top, and is it going down" — and that has to survive the browser cancelling
/// the pointer, the finger leaving the element, and a tap that was never a drag at all.
#[derive(Clone, Copy, Default)]
pub struct Drag {
    origin: Option<(f64, f64)>,
    travel: f64,
    pulling: bool,
}

impl Drag {
    /// The finger landed, `at_top` being the grid's scroll container reporting offset 0. A list
    /// that is already scrolled has its own use for a downward drag.
    pub fn down(&mut self, x: f64, y: f64, at_top: bool) {
        *self = Self::default();
        if at_top {
            self.origin = Some((x, y));
        }
    }

    /// The finger moved: `Some(travel)` once the drag is the pull being watched for, `None`
    /// while it is still a tap or headed somewhere the browser owns. `down` is the direction
    /// that counts — downward for the grid, upward for the sheet.
    pub fn mov(&mut self, x: f64, y: f64, down: bool) -> Option<f64> {
        let (ox, oy) = self.origin?;
        let (dx, dy) = (x - ox, y - oy);
        let along = if down { dy } else { -dy };
        if !self.pulling {
            if dy.abs() < SLOP && dx.abs() < SLOP {
                return None;
            }
            // The other direction belongs to the page, and sideways is not a pull: let go of
            // the pointer and stay out of its way for the rest of the drag.
            if along <= 0.0 || along <= dx.abs() {
                self.origin = None;
                return None;
            }
            self.pulling = true;
        }
        self.travel = along.max(0.0);
        Some(self.travel)
    }

    /// The finger lifted: the travel so far, if this was a pull at all.
    pub fn up(&mut self) -> Option<f64> {
        let travel = self.pulling.then_some(self.travel);
        *self = Self::default();
        travel
    }

    /// The browser took the pointer back (a pan started, a call came in). Whatever the panel was
    /// showing goes back with it.
    pub fn cancel(&mut self) {
        *self = Self::default();
    }
}

/// The grid's scroll offset, in CSS pixels.
///
/// Walked, not assumed: the feed is not the scroll box today (the page is), and the rule "only
/// from offset 0" has to keep holding if the layout ever grows a real scroll box of its own.
pub fn scroll_offset() -> f64 {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return 0.0;
    };
    let mut node = document.get_element_by_id(STRIP);
    while let Some(element) = node {
        // `body`/`html` report their own `scrollTop` as 0 while the document is what scrolls;
        // the scrolling element is the box that actually holds the offset.
        if matches!(element.tag_name().as_str(), "BODY" | "HTML") {
            break;
        }
        if element.scroll_height() > element.client_height() {
            return element.scroll_top() as f64;
        }
        node = element.parent_element();
    }
    document
        .scrolling_element()
        .map_or(0.0, |element| element.scroll_top() as f64)
}
