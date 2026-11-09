//! Live, follow-the-hand Space transitions for desktop-bound gestures.
//!
//! A horizontal swipe bound to Next/Previous Desktop opens a live transition
//! instead of firing the one-shot switch: the Dock drags the neighbouring
//! Space in as the hand keeps moving, every full desktop width of travel
//! completes one switch, and the release finishes the switch (past halfway,
//! or flicked) or lets it spring back — a trackpad's three-finger swipe,
//! driven by the gesture button. A finish carries the hand's speed, so the
//! Dock eases on from where the hand let go.
//!
//! The Dock takes one swipe at a time, so a long sweep settles between
//! desktops: after a switch completes, motion is dropped until the Space
//! change lands, and the next transition starts from zero rather than jumping
//! to where the hand has got to. At the first and last desktop the transition
//! holds at the edge instead of asking for a switch the Dock cannot make.
//!
//! Mission Control and App Exposé play the same way along the trackpad's
//! vertical swipe: the overview opens as the hand moves and the release
//! finishes opening it or lets it fall closed. They never finish mid-hold:
//! the transition holds fully open until the release.
//!
//! Whichever way the hand moves is the bound direction's business: the
//! action picks the Dock gesture, and travel along the bound swipe's own axis
//! drives it, so Mission Control bound to Left still follows the hand.
//!
//! This is pure policy: it turns hold events into frames, and the dispatcher
//! posts them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, ButtonId, GestureDirection};
use openlogi_inject::{SpacePosition, SpaceSwipePhase, SwipeAxis};

use crate::runtime::HidppSessionId;

/// Raw-XY travel that drags the transition across one full desktop, unless
/// [`openlogi_core::env::SPACE_SWIPE_TRAVEL`] overrides it.
pub(super) const DEFAULT_TRAVEL: f64 = 800.0;
/// Progress past which a release finishes the switch instead of springing back.
const COMPLETE_AT: f64 = 0.5;
/// Release speed, in desktop widths per second along the transition, that
/// finishes a switch short of [`COMPLETE_AT`], like a trackpad flick.
const FLICK_SPEED: f64 = 2.5;
/// Least progress a flick needs, so a twitch at the commit never switches.
const FLICK_MIN: f64 = 0.1;
/// A motion gap this long means the hand stopped: the release carries no speed.
const STALE_MOTION: Duration = Duration::from_millis(100);
/// Reports closer together than this move the transition but are no speed
/// sample: a gap that short measures the report timing, not the hand.
const MIN_SPEED_SAMPLE: Duration = Duration::from_millis(2);
/// Longest wait for a completed switch to land before the next transition
/// starts anyway (the Dock may not report a change it did not make). A switch
/// on a large display was seen landing ~0.9 s after its Ended.
const SETTLE_TIMEOUT: Duration = Duration::from_millis(1500);

/// The travel per desktop in effect; `None` when live transitions are off.
pub(super) fn configured_travel() -> Option<f64> {
    let travel = std::env::var(openlogi_core::env::SPACE_SWIPE_TRAVEL)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .unwrap_or(DEFAULT_TRAVEL);
    (travel.is_finite() && travel > 0.0).then_some(travel)
}

/// What the transition asks of the desktop it runs on.
#[derive(Clone, Copy)]
pub(super) struct SpaceProbe {
    /// Where the display under the cursor is in its row of desktops; `None`
    /// when unknown (then both neighbours are assumed and a landing is read
    /// from `changes` alone).
    pub(super) position: fn() -> Option<SpacePosition>,
    /// A count that ticks when any display's active desktop changes; `None`
    /// when the platform does not report it.
    pub(super) changes: fn() -> Option<u64>,
}

impl SpaceProbe {
    /// The live desktop.
    #[cfg_attr(test, expect(dead_code, reason = "unit tests read a fake desktop"))]
    pub(super) const NATIVE: Self = Self {
        position: openlogi_inject::space_position,
        changes: openlogi_inject::space_change_count,
    };
}

/// One frame for the Dock: the gesture it plays, cumulative progress
/// (desktop widths positive toward the next desktop, or the fraction open
/// positive toward Mission Control) and its lifecycle phase.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Frame {
    pub(super) axis: SwipeAxis,
    pub(super) progress: f64,
    pub(super) phase: SpaceSwipePhase,
}

impl Frame {
    const fn new(axis: SwipeAxis, progress: f64, phase: SpaceSwipePhase) -> Self {
        Self {
            axis,
            progress,
            phase,
        }
    }
}

/// How far a vertical swipe's `Began` already reaches toward `target`. The
/// Dock picks Mission Control or App Exposé from that first frame and drops
/// a vertical swipe that begins at rest; a trackpad's begins about 0.01 along.
const VERTICAL_BEGAN_PROGRESS: f64 = 0.01;

/// The progress a live swipe's `Began` frame carries along `axis`.
fn began_progress(axis: SwipeAxis, target: f64) -> f64 {
    match axis {
        SwipeAxis::Horizontal => 0.0,
        SwipeAxis::Vertical => target * VERTICAL_BEGAN_PROGRESS,
    }
}

/// Whether a transition along `axis` moves the Space: a desktop switch does,
/// while an overview only opens over it and never ticks the Space probe.
fn switches_desktop(axis: SwipeAxis) -> bool {
    axis == SwipeAxis::Horizontal
}

/// The Dock gesture an action plays live, and its progress sign: `+1` toward
/// the next desktop, or toward Mission Control. Show Desktop is a pinch, not
/// a swipe, and stays one-shot with every other action.
fn live_target(action: &Action) -> Option<(SwipeAxis, f64)> {
    match action {
        Action::NextDesktop => Some((SwipeAxis::Horizontal, 1.0)),
        Action::PreviousDesktop => Some((SwipeAxis::Horizontal, -1.0)),
        Action::MissionControl => Some((SwipeAxis::Vertical, 1.0)),
        Action::AppExpose => Some((SwipeAxis::Vertical, -1.0)),
        _ => None,
    }
}

/// Whether `action` plays as a live swipe on this platform.
pub(crate) fn plays_live(action: &Action) -> bool {
    cfg!(target_os = "macos") && live_target(action).is_some()
}

/// The raw-XY axis a swipe travels along and its sign there: `+1`
/// rightward or downward.
fn swipe_source(direction: GestureDirection) -> Option<(SwipeAxis, f64)> {
    match direction {
        GestureDirection::Right => Some((SwipeAxis::Horizontal, 1.0)),
        GestureDirection::Left => Some((SwipeAxis::Horizontal, -1.0)),
        GestureDirection::Down => Some((SwipeAxis::Vertical, 1.0)),
        GestureDirection::Up => Some((SwipeAxis::Vertical, -1.0)),
        GestureDirection::Click => None,
    }
}

/// Which way a transition may complete from the current desktop.
#[derive(Clone, Copy)]
struct Bounds {
    previous: bool,
    next: bool,
}

impl Bounds {
    /// No edge: an overview opens either way from anywhere.
    const OPEN: Self = Self {
        previous: true,
        next: true,
    };

    fn read(probe: SpaceProbe) -> Self {
        (probe.position)().map_or(
            Self {
                previous: true,
                next: true,
            },
            |position| Self {
                previous: position.has_previous,
                next: position.has_next,
            },
        )
    }

    /// Whether a desktop exists toward progress of `sign`.
    fn allows(self, sign: f64) -> bool {
        if sign > 0.0 { self.next } else { self.previous }
    }
}

/// A completed switch the Dock may still be animating.
#[derive(Clone, Copy)]
struct Landing {
    since: Instant,
    /// The change count when the switch completed.
    changes: Option<u64>,
    /// This display's desktop when the switch completed, so a change on
    /// another display is not mistaken for this one landing.
    space: Option<u64>,
}

impl Landing {
    fn at(probe: SpaceProbe, now: Instant) -> Self {
        Self {
            since: now,
            changes: (probe.changes)(),
            space: (probe.position)().map(|position| position.current),
        }
    }

    /// Whether the switch landed, or waiting for it has timed out. Landed
    /// once a change was reported and this display's own desktop moved on;
    /// either signal alone is not enough on a multi-display desktop.
    fn is_over(self, probe: SpaceProbe, now: Instant) -> bool {
        if now.saturating_duration_since(self.since) >= SETTLE_TIMEOUT {
            return true;
        }
        let ticked = (probe.changes)()
            .zip(self.changes)
            .map(|(current, at_end)| current != at_end);
        let moved = (probe.position)()
            .map(|position| position.current)
            .zip(self.space)
            .map(|(current, at_end)| current != at_end);
        match (ticked, moved) {
            (None, None) => false,
            (ticked, moved) => ticked.unwrap_or(true) && moved.unwrap_or(true),
        }
    }
}

/// How a live transition opened.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Opened {
    /// Post this first frame now.
    Now(Frame),
    /// The last switch is still landing: the Began follows with the first
    /// motion after it lands.
    AfterLanding,
}

impl Opened {
    /// The frame to post now, if any.
    pub(super) fn frame(self) -> Option<Frame> {
        match self {
            Self::Now(frame) => Some(frame),
            Self::AfterLanding => None,
        }
    }
}

enum Stage {
    /// A Dock transition is following the hand.
    InFlight { progress: f64, bounds: Bounds },
    /// A switch completed and is still landing; motion is dropped until it
    /// lands, and the next leg's Began waits for it too: the Dock does not
    /// take a new swipe mid-animation, which then falls through to the app.
    Settling(Landing),
}

struct Transition {
    /// The Dock gesture the transition plays.
    axis: SwipeAxis,
    /// The raw-XY axis the hand drives it along: the bound swipe's own.
    along: SwipeAxis,
    /// Progress per unit of raw travel along `along` (rightward or
    /// downward): `+1` when that heads toward the next desktop or Mission
    /// Control.
    polarity: f64,
    /// The progress sign the hand may not drag back across, when the opposite
    /// swipe is not bound to the opposite desktop action; `None` lets the
    /// transition swing to either neighbour.
    keep_sign: Option<f64>,
    stage: Stage,
    /// Opened by a commit and not moved since: the commit's own travel is
    /// still to come.
    fresh: bool,
    /// The bound action, while the transition waits on the last switch's
    /// landing and no Began has been posted for it: the one-shot fallback
    /// should that Began be refused or the hold end first.
    unstarted: Option<Action>,
    /// Smoothed speed along the transition, desktop widths per second.
    velocity: f64,
    moved_at: Instant,
}

/// Move a leg to `progress`: follow the hand below a full desktop, hold at the
/// edge of the last desktop, or finish the switch at a full width and settle.
/// `velocity` is the hand's speed along the transition, which a finish
/// carries toward its desktop. An overview never finishes mid-hold: it holds
/// fully open until the release.
fn step(
    axis: SwipeAxis,
    progress: f64,
    velocity: f64,
    bounds: Bounds,
    probe: SpaceProbe,
    now: Instant,
) -> (Stage, Frame) {
    let follow = |progress: f64, bounds: Bounds| {
        (
            Stage::InFlight { progress, bounds },
            Frame::new(axis, progress, SpaceSwipePhase::Changed),
        )
    };
    if progress.abs() < 1.0 {
        return follow(progress, bounds);
    }
    let sign = progress.signum();
    if !switches_desktop(axis) {
        return follow(sign, bounds);
    }
    // The edge read at the start can predate a switch WindowServer has not
    // reported yet: read it again before holding there.
    let bounds = if bounds.allows(sign) {
        bounds
    } else {
        Bounds::read(probe)
    };
    if !bounds.allows(sign) {
        return follow(sign, bounds);
    }
    let settling = Stage::Settling(Landing::at(probe, now));
    let speed = velocity * sign;
    (
        settling,
        Frame::new(axis, sign, SpaceSwipePhase::Ended { speed }),
    )
}

/// Live transitions by capture session and held gesture source. A hold keeps
/// its entry from the first desktop swipe until release, so later swipes in
/// the same hold are motion for the transition rather than new actions.
pub(super) struct SpaceSwipes {
    travel: Option<f64>,
    probe: SpaceProbe,
    active: HashMap<(HidppSessionId, ButtonId), Transition>,
    /// The last switch a release finished, until it lands.
    landing: Option<Landing>,
}

impl SpaceSwipes {
    /// Live transitions dragging `travel` raw-XY units per desktop (or none),
    /// reading the desktop through `probe`.
    pub(super) fn new(travel: Option<f64>, probe: SpaceProbe) -> Self {
        Self {
            travel,
            probe,
            active: HashMap::new(),
            landing: None,
        }
    }

    /// Whether `button`'s hold on `session` is driving a live transition.
    pub(super) fn is_active(&self, session: &HidppSessionId, button: ButtonId) -> bool {
        self.active.contains_key(&(session.clone(), button))
    }

    /// Whether `button`'s hold on `session` just opened its transition and
    /// has not moved it yet, so the opening commit's travel is still owed.
    pub(super) fn awaits_commit_travel(&self, session: &HidppSessionId, button: ButtonId) -> bool {
        self.active
            .get(&(session.clone(), button))
            .is_some_and(|t| t.fresh)
    }

    /// Open a live transition for a committed swipe bound to an action with
    /// a native swipe (see [`live_target`]), saying whether it begins at
    /// once; `None` (nothing opened) for anything else. `opposite` is the
    /// action bound to the reverse swipe. It opens at rest: the commit's own
    /// travel follows as the hold's first motion.
    pub(super) fn begin(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        direction: GestureDirection,
        action: &Action,
        opposite: Option<&Action>,
        now: Instant,
    ) -> Option<Opened> {
        // Live transitions are off without a travel per desktop.
        self.travel?;
        let (along, source) = swipe_source(direction)?;
        let (axis, target) = live_target(action)?;
        let swings_both_ways = opposite.and_then(live_target) == Some((axis, -target));
        let stage = if switches_desktop(axis) {
            // Subscribe to Space changes before the first switch can land.
            let _ = (self.probe.changes)();
            match self
                .landing
                .take()
                .filter(|landing| !landing.is_over(self.probe, now))
            {
                Some(landing) => Stage::Settling(landing),
                None => Stage::InFlight {
                    progress: 0.0,
                    bounds: Bounds::read(self.probe),
                },
            }
        } else {
            Stage::InFlight {
                progress: 0.0,
                bounds: Bounds::OPEN,
            }
        };
        let waits = matches!(stage, Stage::Settling(_));
        self.active.insert(
            (session.clone(), button),
            Transition {
                axis,
                along,
                polarity: target * source,
                keep_sign: (!swings_both_ways).then_some(target),
                stage,
                fresh: true,
                unstarted: waits.then(|| action.clone()),
                velocity: 0.0,
                moved_at: now,
            },
        );
        Some(if waits {
            Opened::AfterLanding
        } else {
            Opened::Now(Frame::new(
                axis,
                began_progress(axis, target),
                SpaceSwipePhase::Began,
            ))
        })
    }

    /// The action `button`'s hold is bound to, while its transition waits to
    /// begin and has posted nothing yet.
    pub(super) fn unstarted(&self, session: &HidppSessionId, button: ButtonId) -> Option<Action> {
        self.active
            .get(&(session.clone(), button))
            .and_then(|t| t.unstarted.clone())
    }

    /// Forget a transition whose Began, for its first leg or a later one,
    /// could not be posted.
    pub(super) fn abandon(&mut self, session: &HidppSessionId, button: ButtonId) {
        self.active.remove(&(session.clone(), button));
    }

    /// Spring back the transition `button`'s hold is driving, if any.
    pub(super) fn cancel(&mut self, session: &HidppSessionId, button: ButtonId) -> Vec<Frame> {
        match self.active.remove(&(session.clone(), button)) {
            Some(Transition {
                axis,
                stage: Stage::InFlight { progress, .. },
                ..
            }) => vec![Frame::new(axis, progress, SpaceSwipePhase::Cancelled)],
            Some(Transition {
                stage: Stage::Settling(landing),
                ..
            }) => {
                self.landing = Some(landing);
                Vec::new()
            }
            None => Vec::new(),
        }
    }

    /// Follow one raw-XY report of the hold, returning the frames to post.
    pub(super) fn motion(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        dx: i32,
        dy: i32,
        now: Instant,
    ) -> Vec<Frame> {
        let (Some(travel), probe) = (self.travel, self.probe) else {
            return Vec::new();
        };
        let Some(t) = self.active.get_mut(&(session.clone(), button)) else {
            return Vec::new();
        };
        let commit_travel = std::mem::replace(&mut t.fresh, false);
        let raw = match t.along {
            SwipeAxis::Horizontal => dx,
            SwipeAxis::Vertical => dy,
        };
        let axis = t.axis;
        let delta = t.polarity * f64::from(raw) / travel;
        let elapsed = now.saturating_duration_since(t.moved_at);
        if elapsed >= STALE_MOTION {
            t.velocity = 0.0;
        }
        // The commit's travel built up before the transition opened, over a
        // time this gap does not measure: it moves the transition, but how
        // fast the hand went is learnt from the reports after it.
        if !commit_travel && elapsed >= MIN_SPEED_SAMPLE {
            t.velocity = 0.5 * t.velocity + 0.5 * delta / elapsed.as_secs_f64();
        }
        t.moved_at = now;
        let velocity = t.velocity;
        let keep_sign = t.keep_sign;
        let hold_sign = |progress: f64| match keep_sign {
            Some(sign) if progress * sign < 0.0 => 0.0,
            _ => progress,
        };

        match &mut t.stage {
            Stage::Settling(landing) => {
                if !landing.is_over(probe, now) {
                    return Vec::new();
                }
                // The switch landed: the next transition starts from this
                // report, not from wherever the hand got to meanwhile, and
                // the report gets the same edge and completion checks as any
                // other (a full desktop's worth finishes at once, with no
                // speed sample yet).
                t.velocity = 0.0;
                t.unstarted = None;
                let (stage, frame) =
                    step(axis, hold_sign(delta), 0.0, Bounds::read(probe), probe, now);
                t.stage = stage;
                match frame.phase {
                    SpaceSwipePhase::Ended { .. } => {
                        vec![Frame::new(axis, 0.0, SpaceSwipePhase::Began), frame]
                    }
                    _ => vec![Frame::new(axis, frame.progress, SpaceSwipePhase::Began)],
                }
            }
            Stage::InFlight { progress, bounds } => {
                let (stage, frame) = step(
                    axis,
                    hold_sign(*progress + delta),
                    velocity,
                    *bounds,
                    probe,
                    now,
                );
                t.stage = stage;
                vec![frame]
            }
        }
    }

    /// Release the hold: finish the switch in flight when it is past halfway
    /// or flicked toward a desktop that exists, otherwise let it spring back.
    /// The Dock finishes an Ended from any progress, easing out from the
    /// hand's speed, so a flick ends where the hand let go rather than
    /// jumping ahead first.
    pub(super) fn end(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        now: Instant,
    ) -> Vec<Frame> {
        let Some(t) = self.active.remove(&(session.clone(), button)) else {
            return Vec::new();
        };
        let (progress, bounds) = match t.stage {
            Stage::InFlight { progress, bounds } => (progress, bounds),
            Stage::Settling(landing) => {
                self.landing = Some(landing);
                return Vec::new();
            }
        };
        let sign = progress.signum();
        let speed = if now.saturating_duration_since(t.moved_at) >= STALE_MOTION {
            0.0
        } else {
            t.velocity * sign
        };
        let past_halfway = progress.abs() >= COMPLETE_AT && speed > -FLICK_SPEED;
        let flicked = progress.abs() >= FLICK_MIN && speed >= FLICK_SPEED;
        if bounds.allows(sign) && (past_halfway || flicked) {
            // Gating the next swipe on an overview's landing would just stall.
            if switches_desktop(t.axis) {
                self.landing = Some(Landing::at(self.probe, now));
            }
            vec![Frame::new(
                t.axis,
                progress,
                SpaceSwipePhase::Ended { speed },
            )]
        } else {
            vec![Frame::new(t.axis, progress, SpaceSwipePhase::Cancelled)]
        }
    }

    /// Spring back every transition `session` has in flight.
    pub(super) fn cancel_session(&mut self, session: &HidppSessionId) -> Vec<Frame> {
        let mut frames = Vec::new();
        let mut landing = None;
        self.active.retain(|(candidate, _), t| {
            if candidate != session {
                return true;
            }
            match t.stage {
                Stage::InFlight { progress, .. } => {
                    frames.push(Frame::new(t.axis, progress, SpaceSwipePhase::Cancelled));
                }
                Stage::Settling(settling) => landing = Some(settling),
            }
            false
        });
        if landing.is_some() {
            self.landing = landing;
        }
        frames
    }
}

#[cfg(test)]
mod tests;
