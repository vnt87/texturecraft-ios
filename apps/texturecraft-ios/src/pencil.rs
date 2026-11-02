//! Apple Pencil input: which touch is the pen, and the pencil's double-tap.
//!
//! Pressure already arrives for free — winit forwards an Apple Pencil touch as a
//! `WindowEvent::Touch` with a calibrated force and egui-winit normalises it — so the gap this
//! boundary fills is *identity*: egui cannot tell a pen from a finger, drives its emulated pointer
//! from the first contact down, and counts a resting palm as a pinch. UIKit knows, and says so
//! before winit ever sees the touch.
//!
//! Installed on the window winit created (the window, not winit's view, so nothing here depends on
//! winit's private view class):
//!
//! - `TouchObserver`, a `UIGestureRecognizer` subclass that never recognizes and never delays or
//!   cancels touches. UIKit delivers each touch to a window's gesture recognizers before the
//!   hit-tested view, so a Pencil touch is marked as a pen in [`texturecraft_ui_egui::stylus`]
//!   before winit turns it into an egui touch.
//! - A `UIPencilInteraction` whose delegate reports a double-tap, honouring the user's
//!   Settings › Apple Pencil › Double Tap choice, and asks egui for a frame so the swap shows at once.
//!
//! UNVERIFIED (needs a simulator or an iPad with a Pencil): that the observer sees the touch before
//! winit's view, and that it never delays winit's touch ends. If the ordering is wrong the pen still
//! paints — at its own pressure, through the pointer egui emulates — it simply gains no palm
//! rejection. See the checklist in `apps/texturecraft-ios/README.md`.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSObject, NSObjectProtocol, NSSet};
use objc2_ui_kit::{
    UIEvent, UIGestureRecognizer, UIGestureRecognizerState, UIPencilInteraction, UIPencilInteractionDelegate, UIPencilInteractionTap, UITouch, UITouchPhase,
    UITouchType, UIWindow,
};
use texturecraft_ui_egui::stylus;

/// Objects UIKit holds weakly (the interaction's delegate), plus what must outlive the window's use
/// of them and the egui context a double-tap repaints.
struct Installed {
    _observer: Retained<TouchObserver>,
    _interaction: Retained<UIPencilInteraction>,
    _target: Retained<PencilTarget>,
    ctx: egui::Context,
}

thread_local! {
    static INSTALLED: RefCell<Option<Installed>> = const { RefCell::new(None) };
}

/// The id egui-winit gives this touch: winit 0.30.13 uses the `UITouch` address
/// (`touch as *const UITouch as u64`, `platform_impl/ios/view.rs`).
fn touch_id(touch: &UITouch) -> u64 {
    std::ptr::from_ref(touch) as usize as u64
}

#[derive(Clone, Copy)]
enum Phase {
    Began,
    Ended,
}

/// Tell the shell what each contact in this batch is. Only a pen's contacts are followed, and the
/// end of a contact is read from egui's copy of the same touch, so nothing is kept past the call.
fn observe(touches: &NSSet<UITouch>, phase: Phase) {
    let feed = stylus::feed();
    for touch in touches.iter() {
        let id = touch_id(&touch);
        match (phase, touch.r#type() == UITouchType::Pencil) {
            (Phase::Began, true) => feed.pen_began(id),
            (Phase::Began, false) => feed.finger_began(id),
            (Phase::Ended, true) => feed.pen_ended(id),
            (Phase::Ended, false) => {}
        }
    }
}

/// Settings › Apple Pencil › Double Tap: 0 ignores, 1 and 2 swap to the eraser and back.
fn double_tapped(mtm: MainThreadMarker) {
    if !matches!(UIPencilInteraction::preferredTapAction(mtm).0, 1 | 2) {
        return;
    }
    stylus::feed().tap();
    // A double-tap moves nothing, so egui would not otherwise draw the new tool until the next input.
    if let Some(ctx) = INSTALLED.with(|i| i.try_borrow().ok().and_then(|slot| slot.as_ref().map(|s| s.ctx.clone()))) {
        ctx.request_repaint();
    }
}

define_class!(
    // SAFETY: UIGestureRecognizer may be subclassed (UIGestureRecognizerSubclass.h); this subclass
    // adds no ivars, is main-thread only like UIKit, and every override calls `super` with the
    // arguments it received, as UIKit requires.
    #[unsafe(super(UIGestureRecognizer, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TextureCraftPencilTouchObserver"]
    struct TouchObserver;

    impl TouchObserver {
        // SAFETY (all four): UIKit calls these on the main thread with a valid NSSet of UITouch and
        // an event that is borrowed for the duration of the call (taken as `Option` in case UIKit
        // passes nil). Nothing here is retained past the call.
        #[unsafe(method(touchesBegan:withEvent:))]
        fn touches_began(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            // SAFETY: forwards the exact arguments to the superclass implementation.
            let _: () = unsafe { msg_send![super(self), touchesBegan: touches, withEvent: event] };
            observe(touches, Phase::Began);
        }

        #[unsafe(method(touchesMoved:withEvent:))]
        fn touches_moved(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            // SAFETY: forwards the exact arguments to the superclass implementation.
            let _: () = unsafe { msg_send![super(self), touchesMoved: touches, withEvent: event] };
        }

        #[unsafe(method(touchesEnded:withEvent:))]
        fn touches_ended(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            // SAFETY: forwards the exact arguments to the superclass implementation.
            let _: () = unsafe { msg_send![super(self), touchesEnded: touches, withEvent: event] };
            observe(touches, Phase::Ended);
            self.fail_when_idle(event);
        }

        #[unsafe(method(touchesCancelled:withEvent:))]
        fn touches_cancelled(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            // SAFETY: forwards the exact arguments to the superclass implementation.
            let _: () = unsafe { msg_send![super(self), touchesCancelled: touches, withEvent: event] };
            observe(touches, Phase::Ended);
            self.fail_when_idle(event);
        }
    }
);

impl TouchObserver {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: `init` is UIGestureRecognizer's inherited initializer (target and action nil);
        // the class has no ivars to initialize. Runs on the main thread (`mtm`).
        let this: Retained<Self> = unsafe { msg_send![Self::alloc(mtm), init] };
        // A pure observer: never hold back or cancel the touches winit's view receives.
        this.setCancelsTouchesInView(false);
        this.setDelaysTouchesBegan(false);
        this.setDelaysTouchesEnded(false);
        this
    }

    /// Once every touch of the sequence is over, leave `Possible` so UIKit resets the recognizer for
    /// the next sequence instead of keeping it pending.
    fn fail_when_idle(&self, event: Option<&UIEvent>) {
        let live =
            event.and_then(UIEvent::allTouches).is_some_and(|all| all.iter().any(|t| !matches!(t.phase(), UITouchPhase::Ended | UITouchPhase::Cancelled)));
        if !live {
            self.setState(UIGestureRecognizerState::Failed);
        }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no ivars; main-thread only.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TextureCraftPencilTarget"]
    struct PencilTarget;

    // SAFETY: NSObjectProtocol has no additional invariants.
    unsafe impl NSObjectProtocol for PencilTarget {}

    // SAFETY: UIKit calls the delegate on the main thread; the interaction and tap objects are
    // borrowed for the call only. When both are implemented, iOS 17.5+ calls only
    // `pencilInteraction:didReceiveTap:` (UIPencilInteraction.h), so a tap is never handled twice.
    unsafe impl UIPencilInteractionDelegate for PencilTarget {
        #[unsafe(method(pencilInteractionDidTap:))]
        fn did_tap(&self, _interaction: &UIPencilInteraction) {
            double_tapped(self.mtm());
        }

        #[unsafe(method(pencilInteraction:didReceiveTap:))]
        fn did_receive_tap(&self, _interaction: &UIPencilInteraction, _tap: &UIPencilInteractionTap) {
            double_tapped(self.mtm());
        }
    }
);

impl PencilTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: NSObject's `init` on a freshly allocated instance without ivars, main thread.
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

/// Attach Apple Pencil input to the app's window. Call once on the main thread after winit created
/// the window (eframe's app-creation callback), with the context to repaint on a double-tap. A
/// second call is a no-op.
pub fn install(ctx: &egui::Context) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("Apple Pencil input must be installed on the main thread")?;
    if INSTALLED.with(|i| i.borrow().is_some()) {
        return Ok(());
    }
    let window: Retained<UIWindow> = crate::scenes::window(mtm).ok_or("No UIKit window to attach Apple Pencil input to")?;
    let observer = TouchObserver::new(mtm);
    window.addGestureRecognizer(&observer);

    let target = PencilTarget::new(mtm);
    let interaction = UIPencilInteraction::new(mtm);
    // UIKit holds the delegate weakly; `target` lives in `INSTALLED`.
    interaction.setDelegate(Some(ProtocolObject::from_ref(&*target)));
    window.addInteraction(ProtocolObject::from_ref(&*interaction));

    INSTALLED.with(|i| {
        if let Ok(mut slot) = i.try_borrow_mut() {
            *slot = Some(Installed { _observer: observer, _interaction: interaction, _target: target, ctx: ctx.clone() });
        }
    });
    log::info!("Apple Pencil input installed (pen identity, double-tap tool swap)");
    Ok(())
}
