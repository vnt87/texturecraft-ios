//! Apple Pencil identity for the touch pipeline.
//!
//! winit forwards an Apple Pencil as an ordinary `WindowEvent::Touch` and egui-winit turns it into
//! `egui::Event::Touch` carrying a normalised force, so pen *pressure* already reaches the brush.
//! What the platform cannot tell egui is *which* contact is the pen: a palm resting on the glass
//! looks like any other finger, egui's two-finger gestures count it, and the pointer egui emulates
//! follows the first touch down, not the pen. This module keeps the two facts the iPadOS runner
//! learns from UIKit — that a contact is a pen, and that the pencil was double-tapped — so the
//! viewport can paint with the pen and ignore everything else touching the screen.
//!
//! [`PenFeed`] is process-wide: the runner's audited UIKit boundary
//! (`apps/texturecraft-ios/src/pencil`) writes into [`feed`], and [`pen_motion`] reads the pen's
//! frames back out of egui's raw input. Every other platform leaves the feed untouched, so the
//! mouse and finger paths behave exactly as before.

use std::sync::{Arc, Mutex, PoisonError};

/// One frame of pen motion, in egui points (the viewport converts to its own space).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenMotion {
    /// Where the tip is.
    pub pos: egui::Pos2,
    /// Force normalised to 0..=1; 1.0 when the screen reports none.
    pub force: f32,
    /// The contact touched down this frame.
    pub began: bool,
    /// The contact lifted (or was cancelled) this frame.
    pub ended: bool,
}

#[derive(Debug, Default)]
struct State {
    /// The pen contact currently down.
    down: Option<u64>,
    /// The pen contact UIKit reported as lifted, kept until a frame reads its end: UIKit delivers
    /// the lift before winit hands the touch to egui, so the stroke has to close on egui's frame.
    lifted: Option<u64>,
    /// A pencil double-tap the shell has not taken yet.
    tap: bool,
    /// The egui frame whose pen transitions were already taken — egui can run one frame in
    /// several passes, and a second `began` would restart the stroke the first one started.
    frame: Option<u64>,
}

/// Pen contacts and double-taps reported by the platform, shared with the shell.
#[derive(Clone, Debug, Default)]
pub struct PenFeed(Arc<Mutex<State>>);

impl PenFeed {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// UIKit saw contact `id` start as a pen. `id` is the address of the `UITouch`, which is the
    /// id winit and egui give the same contact.
    pub fn pen_began(&self, id: u64) {
        let mut s = self.lock();
        s.down = Some(id);
        s.lifted = None;
    }

    /// UIKit saw contact `id` lift or cancel.
    pub fn pen_ended(&self, id: u64) {
        let mut s = self.lock();
        if s.down == Some(id) {
            s.down = None;
            s.lifted = Some(id);
        }
    }

    /// UIKit saw contact `id` start as a finger: the pen is not what is painting.
    pub fn finger_began(&self, id: u64) {
        let mut s = self.lock();
        if s.lifted == Some(id) {
            s.lifted = None;
        }
    }

    /// The pencil was double-tapped (Settings › Apple Pencil › Double-Tap preference).
    pub fn tap(&self) {
        self.lock().tap = true;
    }

    /// Take a waiting double-tap, at most once.
    pub fn take_tap(&self) -> bool {
        let mut s = self.lock();
        std::mem::take(&mut s.tap)
    }

    /// Is the pen touching the screen right now?
    pub fn is_down(&self) -> bool {
        self.lock().down.is_some()
    }

    /// The contact whose pen events the shell should read this frame.
    fn contact(&self) -> Option<u64> {
        let s = self.lock();
        s.down.or(s.lifted)
    }

    /// The lifted pen contact's end has been read: stop looking for it (platforms reuse `UITouch`
    /// addresses, so a later finger must not inherit the pen's id).
    fn lifted_read(&self, id: u64) {
        let mut s = self.lock();
        if s.lifted == Some(id) {
            s.lifted = None;
        }
    }

    /// True the first time it is asked about an egui frame, false for the passes after it.
    fn first_pass(&self, frame: u64) -> bool {
        self.lock().frame.replace(frame) != Some(frame)
    }

    /// The pen's motion in egui's input this frame, `None` when no pen contact was reported.
    ///
    /// The position and force come from egui's raw touch events, which is where winit put them:
    /// the shell never reads UIKit again, so it needs neither the window's scale nor its origin.
    pub fn motion(&self, ctx: &egui::Context) -> Option<PenMotion> {
        let id = self.contact()?;
        let mut m = ctx.input(|i| {
            let mut m: Option<PenMotion> = None;
            for e in &i.raw.events {
                let egui::Event::Touch { id: touch, phase, pos, force, .. } = e else { continue };
                if touch.0 != id {
                    continue;
                }
                let force = force.filter(|f| f.is_finite()).map(|f| f.clamp(0.0, 1.0)).unwrap_or(1.0);
                m = Some(PenMotion {
                    pos: *pos,
                    force,
                    began: matches!(phase, egui::TouchPhase::Start) || m.is_some_and(|p| p.began),
                    ended: matches!(phase, egui::TouchPhase::End | egui::TouchPhase::Cancel),
                });
            }
            m
        })?;
        if m.ended {
            self.lifted_read(id);
        }
        // Later passes of the same frame see the tip's position, never its transitions again.
        if !self.first_pass(ctx.cumulative_frame_nr()) {
            m.began = false;
            m.ended = false;
        }
        Some(m)
    }
}

static FEED: std::sync::OnceLock<PenFeed> = std::sync::OnceLock::new();

/// The feed every platform boundary writes into and the shell reads.
pub fn feed() -> PenFeed {
    FEED.get_or_init(PenFeed::default).clone()
}

/// Is a pen contact down (so fingers on the glass are a resting palm, not a gesture)?
pub fn pen_is_down() -> bool {
    feed().is_down()
}

/// [`PenFeed::motion`] for the feed the platform boundary installed.
pub fn pen_motion(ctx: &egui::Context) -> Option<PenMotion> {
    feed().motion(ctx)
}

/// [`PenFeed::take_tap`] for the feed the platform boundary installed.
pub fn take_pen_tap() -> bool {
    feed().take_tap()
}

/// Touch press-and-hold is off while a pen contact is down, back on for fingers.
///
/// egui takes a touch contact held still past `max_click_duration` (0.8 s) for the screen's
/// right-click, which drops the drag and opens a context menu. On an iPad that is a pen stopped
/// mid-stroke, and a hovering pen never touches the glass at all; Photoshop has no such gesture.
pub fn sync_pen_hold(ctx: &egui::Context) {
    let default = egui::Options::default().input_options.max_click_duration;
    let want = if pen_is_down() { f64::INFINITY } else { default };
    if ctx.options(|o| o.input_options.max_click_duration) != want {
        ctx.options_mut(|o| o.input_options.max_click_duration = want);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pen_contact_is_the_one_that_paints() {
        let feed = PenFeed::default();
        assert!(!feed.is_down());
        feed.pen_began(7);
        assert!(feed.is_down());
        feed.pen_ended(7);
        assert!(!feed.is_down(), "the pen lifted");
        // The lift stays readable for exactly one frame, then the id is retired.
        assert_eq!(feed.contact(), Some(7));
        feed.lifted_read(7);
        assert_eq!(feed.contact(), None);
    }

    #[test]
    fn a_new_pen_replaces_a_contact_no_frame_ever_read() {
        let feed = PenFeed::default();
        feed.pen_began(7);
        feed.pen_ended(7);
        feed.pen_began(9);
        assert_eq!(feed.contact(), Some(9));
        assert!(feed.is_down());
    }

    #[test]
    fn a_finger_does_not_inherit_the_pen_s_contact() {
        let feed = PenFeed::default();
        feed.pen_began(7);
        feed.pen_ended(7);
        feed.finger_began(7);
        assert_eq!(feed.contact(), None);
    }

    #[test]
    fn a_double_tap_is_taken_once() {
        let feed = PenFeed::default();
        assert!(!feed.take_tap());
        feed.tap();
        assert!(feed.take_tap());
        assert!(!feed.take_tap());
    }

    #[test]
    fn pen_transitions_are_read_once_per_frame() {
        let feed = PenFeed::default();
        assert!(feed.first_pass(3));
        assert!(!feed.first_pass(3));
        assert!(feed.first_pass(4));
    }

    #[test]
    fn a_pen_contact_reads_its_frames() {
        let ctx = egui::Context::default();
        let feed = PenFeed::default();
        let touch = |id: u64, phase, x: f32, force| egui::Event::Touch {
            device_id: egui::TouchDeviceId(1),
            id: egui::TouchId(id),
            phase,
            pos: egui::pos2(x, 10.0),
            force,
        };
        let frame = |events| egui::RawInput { events, ..Default::default() };
        feed.pen_began(3);
        // A press and two moves delivered in one frame: the tip is where it last was, the stroke began.
        let mut out = ctx.run_ui(
            frame(vec![
                touch(3, egui::TouchPhase::Start, 1.0, Some(0.2)),
                touch(3, egui::TouchPhase::Move, 4.0, Some(0.6)),
                touch(5, egui::TouchPhase::Start, 20.0, None),
            ]),
            |ui| {
                assert_eq!(feed.motion(ui.ctx()), Some(PenMotion { pos: egui::pos2(4.0, 10.0), force: 0.6, began: true, ended: false }));
                // A later pass of the same frame must not begin the stroke a second time.
                assert_eq!(feed.motion(ui.ctx()), Some(PenMotion { pos: egui::pos2(4.0, 10.0), force: 0.6, began: false, ended: false }));
            },
        );
        out.textures_delta.clear();
        // UIKit reported the lift before egui did: the end arrives with the frame that reads it.
        feed.pen_ended(3);
        let mut out = ctx.run_ui(frame(vec![touch(3, egui::TouchPhase::End, 9.0, None)]), |ui| {
            assert_eq!(feed.motion(ui.ctx()).map(|m| (m.pos.x, m.force, m.ended)), Some((9.0, 1.0, true)));
            assert_eq!(feed.motion(ui.ctx()), None, "the retired contact is not read again");
        });
        out.textures_delta.clear();
    }

    #[test]
    fn a_finger_only_frame_reports_no_pen() {
        let ctx = egui::Context::default();
        let feed = PenFeed::default();
        let mut out = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Touch {
                    device_id: egui::TouchDeviceId(1),
                    id: egui::TouchId(8),
                    phase: egui::TouchPhase::Start,
                    pos: egui::pos2(1.0, 2.0),
                    force: None,
                }],
                ..Default::default()
            },
            |ui| assert_eq!(feed.motion(ui.ctx()), None),
        );
        out.textures_delta.clear();
    }
}
