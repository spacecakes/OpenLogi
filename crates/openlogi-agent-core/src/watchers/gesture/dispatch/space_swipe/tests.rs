use std::cell::Cell;

use super::*;

const TRAVEL: f64 = 800.0;
const BUTTON: ButtonId = ButtonId::GestureButton;

thread_local! {
    static TICKS: Cell<u64> = const { Cell::new(0) };
    static HAS_NEXT: Cell<bool> = const { Cell::new(true) };
}

/// A counter that moves on every read: a desktop that changes at once.
fn tick() -> u64 {
    TICKS.set(TICKS.get() + 1);
    TICKS.get()
}

const fn middle(current: u64) -> SpacePosition {
    SpacePosition {
        current,
        has_previous: true,
        has_next: true,
    }
}

/// A desktop with neighbours both ways whose switches land at once.
const PROMPT: SpaceProbe = SpaceProbe {
    position: || Some(middle(tick())),
    changes: || Some(tick()),
};

/// A desktop whose switches never report landing.
const SILENT: SpaceProbe = SpaceProbe {
    position: || Some(middle(7)),
    changes: || Some(0),
};

/// Another display keeps changing Spaces; this one never does.
const ELSEWHERE: SpaceProbe = SpaceProbe {
    position: || Some(middle(7)),
    changes: || Some(tick()),
};

/// The last desktop: nothing after it.
const LAST: SpaceProbe = SpaceProbe {
    position: || {
        Some(SpacePosition {
            current: 7,
            has_previous: true,
            has_next: false,
        })
    },
    changes: || Some(0),
};

/// A desktop whose switches never report landing, with a next desktop only
/// while [`HAS_NEXT`] says so.
const SILENT_UNTIL_LAST: SpaceProbe = SpaceProbe {
    position: || {
        Some(SpacePosition {
            current: 7,
            has_previous: true,
            has_next: HAS_NEXT.get(),
        })
    },
    changes: || Some(0),
};

fn session() -> HidppSessionId {
    HidppSessionId::with_epoch("mouse", 1)
}

fn swipes(probe: SpaceProbe) -> SpaceSwipes {
    SpaceSwipes::new(Some(TRAVEL), probe)
}

/// Open the usual Right → Next / Left → Previous transition.
fn begin_right(swipes: &mut SpaceSwipes, now: Instant) -> Frame {
    swipes
        .begin(
            &session(),
            BUTTON,
            GestureDirection::Right,
            &Action::NextDesktop,
            Some(&Action::PreviousDesktop),
            now,
        )
        .expect("a desktop swipe opens a transition")
        .frame()
        .expect("no switch is landing")
}

fn phases(frames: &[Frame]) -> Vec<SpaceSwipePhase> {
    frames.iter().map(|frame| frame.phase).collect()
}

/// The hand speed an Ended frame carries; `None` for any other phase.
fn end_speed(frame: &Frame) -> Option<f64> {
    match frame.phase {
        SpaceSwipePhase::Ended { speed } => Some(speed),
        SpaceSwipePhase::Began | SpaceSwipePhase::Changed | SpaceSwipePhase::Cancelled => None,
    }
}

/// Feed `reports` motion reports of `dx`, one every 8 ms from `start`.
fn sweep(swipes: &mut SpaceSwipes, start: Instant, reports: u32, dx: i32) -> (Vec<Frame>, Instant) {
    let mut frames = Vec::new();
    let mut now = start;
    for _ in 0..reports {
        now += Duration::from_millis(8);
        frames.extend(swipes.motion(&session(), BUTTON, dx, 0, now));
    }
    (frames, now)
}

#[test]
fn only_swipes_bound_to_a_native_swipe_open_a_transition() {
    let mut swipes = swipes(PROMPT);
    let now = Instant::now();
    for (direction, action) in [
        (GestureDirection::Click, Action::NextDesktop),
        (GestureDirection::Click, Action::MissionControl),
        (GestureDirection::Right, Action::ShowDesktop),
        (GestureDirection::Up, Action::Copy),
    ] {
        assert_eq!(
            swipes.begin(&session(), BUTTON, direction, &action, None, now),
            None
        );
    }
    assert!(!swipes.is_active(&session(), BUTTON));
    assert!(
        SpaceSwipes::new(None, PROMPT)
            .begin(
                &session(),
                BUTTON,
                GestureDirection::Right,
                &Action::NextDesktop,
                None,
                now,
            )
            .is_none(),
        "live transitions off means the one-shot switch"
    );
}

/// Open `action` bound to `direction`, with nothing on the reverse swipe.
fn begin_bound(
    swipes: &mut SpaceSwipes,
    direction: GestureDirection,
    action: &Action,
    now: Instant,
) -> Frame {
    swipes
        .begin(&session(), BUTTON, direction, action, None, now)
        .expect("a native swipe opens a transition")
        .frame()
        .expect("no switch is landing")
}

#[test]
fn any_swipe_direction_drives_its_action_along_its_own_axis() {
    let now = Instant::now();
    let later = now + Duration::from_millis(8);
    for (direction, action, dx, dy, axis, sign) in [
        (
            GestureDirection::Up,
            Action::MissionControl,
            0,
            -80,
            SwipeAxis::Vertical,
            1.0,
        ),
        (
            GestureDirection::Down,
            Action::AppExpose,
            0,
            80,
            SwipeAxis::Vertical,
            -1.0,
        ),
        (
            GestureDirection::Left,
            Action::MissionControl,
            -80,
            0,
            SwipeAxis::Vertical,
            1.0,
        ),
        (
            GestureDirection::Up,
            Action::NextDesktop,
            0,
            -80,
            SwipeAxis::Horizontal,
            1.0,
        ),
        (
            GestureDirection::Down,
            Action::PreviousDesktop,
            0,
            80,
            SwipeAxis::Horizontal,
            -1.0,
        ),
    ] {
        let mut swipes = swipes(SILENT);
        assert_eq!(
            begin_bound(&mut swipes, direction, &action, now),
            Frame::new(axis, began_progress(axis, sign), SpaceSwipePhase::Began)
        );
        // Travel across the bound axis moves nothing.
        let across = swipes.motion(&session(), BUTTON, dy, dx, now);
        assert!(
            across.iter().all(|f| f.progress == 0.0),
            "{direction:?} {action:?}"
        );
        let frames = swipes.motion(&session(), BUTTON, dx, dy, later);
        assert_eq!(frames.len(), 1, "{direction:?} {action:?}");
        assert_eq!(frames[0].axis, axis);
        assert!(
            (frames[0].progress - sign * 80.0 / TRAVEL).abs() < 1e-9,
            "{direction:?} {action:?}: {}",
            frames[0].progress
        );
    }
}

#[test]
fn an_overview_holds_fully_open_until_the_release() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_bound(
        &mut swipes,
        GestureDirection::Up,
        &Action::MissionControl,
        start,
    );
    let (frames, now) = sweep_y(&mut swipes, start, 20, -100);
    assert!(
        frames
            .iter()
            .all(|f| f.phase == SpaceSwipePhase::Changed && f.axis == SwipeAxis::Vertical)
    );
    assert_eq!(frames.last().map(|f| f.progress), Some(1.0));
    let frames = swipes.end(&session(), BUTTON, now + STALE_MOTION * 2);
    assert_eq!(
        frames,
        vec![Frame::new(
            SwipeAxis::Vertical,
            1.0,
            SpaceSwipePhase::Ended { speed: 0.0 }
        )]
    );
    assert!(!swipes.is_active(&session(), BUTTON));
}

#[test]
fn an_overview_released_short_of_halfway_falls_closed() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_bound(
        &mut swipes,
        GestureDirection::Down,
        &Action::AppExpose,
        start,
    );
    let (_, now) = sweep_y(&mut swipes, start, 2, 100);
    let frames = swipes.end(&session(), BUTTON, now + STALE_MOTION * 2);
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Cancelled]);
    assert_eq!(frames[0].axis, SwipeAxis::Vertical);
}

#[test]
fn an_overview_swings_both_ways_only_against_its_opposite() {
    let now = Instant::now();
    let mut paired = swipes(PROMPT);
    paired
        .begin(
            &session(),
            BUTTON,
            GestureDirection::Up,
            &Action::MissionControl,
            Some(&Action::AppExpose),
            now,
        )
        .expect("opened");
    let frames = paired.motion(&session(), BUTTON, 0, 80, now);
    assert!(frames[0].progress < 0.0, "pulling back opens App Exposé");

    let mut alone = swipes(PROMPT);
    alone
        .begin(
            &session(),
            BUTTON,
            GestureDirection::Up,
            &Action::MissionControl,
            Some(&Action::ShowDesktop),
            now,
        )
        .expect("opened");
    let frames = alone.motion(&session(), BUTTON, 0, 80, now);
    assert!(
        frames.iter().all(|f| f.progress >= 0.0),
        "nothing to pull back into"
    );
}

/// Feed `reports` motion reports of `dy`, one every 8 ms from `start`.
fn sweep_y(
    swipes: &mut SpaceSwipes,
    start: Instant,
    reports: u32,
    dy: i32,
) -> (Vec<Frame>, Instant) {
    let mut frames = Vec::new();
    let mut now = start;
    for _ in 0..reports {
        now += Duration::from_millis(8);
        frames.extend(swipes.motion(&session(), BUTTON, 0, dy, now));
    }
    (frames, now)
}

#[test]
fn a_transition_opens_at_zero_and_the_commit_travel_moves_it() {
    let now = Instant::now();
    let mut opened = swipes(PROMPT);
    let frame = begin_right(&mut opened, now);
    assert_eq!(
        frame,
        Frame::new(SwipeAxis::Horizontal, 0.0, SpaceSwipePhase::Began)
    );
    // The commit's own travel arrives as the hold's first motion.
    let frames = opened.motion(&session(), BUTTON, 60, 0, now);
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Changed]);
    assert!((frames[0].progress - 60.0 / TRAVEL).abs() < 1e-9);

    // A swapped binding (Right → Previous) drags toward the previous desktop.
    let mut swapped = swipes(PROMPT);
    swapped
        .begin(
            &session(),
            BUTTON,
            GestureDirection::Right,
            &Action::PreviousDesktop,
            Some(&Action::NextDesktop),
            now,
        )
        .expect("opened");
    let frames = swapped.motion(&session(), BUTTON, 80, 0, now + Duration::from_millis(8));
    assert!(frames[0].progress < 0.0, "moving right heads previous");
}

#[test]
fn the_commit_travel_is_no_speed_sample() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    // The commit's travel lands in the same instant the transition opens:
    // it moves the Space but must not read as an instant flick.
    let _ = swipes.motion(&session(), BUTTON, 60, 0, start);
    let frames = swipes.end(&session(), BUTTON, start + Duration::from_millis(4));
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Cancelled]);
}

#[test]
fn a_late_commit_travel_is_no_speed_sample_either() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    // Reading the Space and posting `Began` took 10 ms, so the commit's travel
    // arrives well past the speed-sample gap. It was accumulated at the hand's
    // own pace, not in this gap: 100 units over 10 ms would read as a flick.
    let _ = swipes.motion(
        &session(),
        BUTTON,
        100,
        0,
        start + Duration::from_millis(10),
    );
    let frames = swipes.end(&session(), BUTTON, start + Duration::from_millis(14));
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Cancelled]);
}

#[test]
fn a_long_sweep_completes_one_switch_per_desktop_width_and_restarts_from_zero() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    // 25 × 100 = 2500 units.
    let (frames, _) = sweep(&mut swipes, start, 25, 100);
    let speeds: Vec<f64> = frames.iter().filter_map(end_speed).collect();
    assert_eq!(speeds.len(), 3);
    // Each switch finishes at the hand's speed: 100 units per 8 ms is about
    // 15.6 desktops per second.
    assert!(speeds.iter().all(|speed| *speed > 10.0), "{speeds:?}");
    // Every next transition starts from the one report that follows the
    // landing — never from the hand's travel during the switch.
    for pair in frames.windows(2) {
        if end_speed(&pair[0]).is_some() {
            assert_eq!(pair[1].phase, SpaceSwipePhase::Began);
            assert!((pair[1].progress - 100.0 / TRAVEL).abs() < 1e-9);
        }
    }
}

#[test]
fn motion_waits_for_the_switch_to_land_then_restarts() {
    let mut swipes = swipes(SILENT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    // Cross one desktop (8 × 100 = 800 units).
    let (frames, now) = sweep(&mut swipes, start, 8, 100);
    assert!(frames.last().and_then(end_speed).is_some(), "{frames:?}");
    // The Dock never reports landing: motion inside the timeout is dropped.
    let (frames, now) = sweep(&mut swipes, now, 10, 100);
    assert!(frames.is_empty(), "no frames while the switch lands");
    // Past the timeout the next transition starts from this report alone.
    let later = now + SETTLE_TIMEOUT;
    let frames = swipes.motion(&session(), BUTTON, 40, 0, later);
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Began]);
    assert!((frames[0].progress - 40.0 / TRAVEL).abs() < 1e-9);
    // A release while settling has nothing in flight.
    let mut settling = swipes_settling(start);
    assert!(settling.end(&session(), BUTTON, start).is_empty());
}

#[test]
fn a_full_desktop_report_after_landing_finishes_at_once() {
    let mut swipes = swipes(SILENT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let (_, now) = sweep(&mut swipes, start, 8, 100);
    // A single report worth more than a desktop opens the next leg and
    // finishes it, rather than leaving a Began past full width. The leg has
    // no speed sample yet.
    let frames = swipes.motion(&session(), BUTTON, 900, 0, now + SETTLE_TIMEOUT);
    assert_eq!(
        frames,
        vec![
            Frame::new(SwipeAxis::Horizontal, 0.0, SpaceSwipePhase::Began),
            Frame::new(
                SwipeAxis::Horizontal,
                1.0,
                SpaceSwipePhase::Ended { speed: 0.0 }
            ),
        ]
    );
}

#[test]
fn a_full_desktop_report_after_landing_on_the_last_desktop_holds_at_the_edge() {
    HAS_NEXT.set(true);
    let mut swipes = swipes(SILENT_UNTIL_LAST);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let (_, now) = sweep(&mut swipes, start, 8, 100);
    // The switch landed on the last desktop.
    HAS_NEXT.set(false);
    let later = now + SETTLE_TIMEOUT;
    let frames = swipes.motion(&session(), BUTTON, 900, 0, later);
    assert_eq!(
        frames,
        vec![Frame::new(
            SwipeAxis::Horizontal,
            1.0,
            SpaceSwipePhase::Began
        )]
    );
    // Pulling back responds at once: there is no excess travel to undo.
    let frames = swipes.motion(
        &session(),
        BUTTON,
        -100,
        0,
        later + Duration::from_millis(8),
    );
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Changed]);
    assert!((frames[0].progress - (1.0 - 100.0 / TRAVEL)).abs() < 1e-9);
}

fn begin_again(swipes: &mut SpaceSwipes, now: Instant) -> Option<Frame> {
    swipes
        .begin(
            &session(),
            BUTTON,
            GestureDirection::Right,
            &Action::NextDesktop,
            Some(&Action::PreviousDesktop),
            now,
        )
        .expect("a desktop swipe opens a transition")
        .frame()
}

#[test]
fn a_hold_right_after_a_finished_switch_waits_for_it_to_land() {
    let mut swipes = swipes(SILENT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let (_, now) = sweep(&mut swipes, start, 45, 10);
    let frames = swipes.end(&session(), BUTTON, now + Duration::from_millis(300));
    assert!(frames.first().and_then(end_speed).is_some(), "{frames:?}");
    // The next hold opens without a Began while the Dock still animates.
    let now = now + Duration::from_millis(400);
    assert_eq!(begin_again(&mut swipes, now), None);
    let (frames, now) = sweep(&mut swipes, now, 10, 10);
    assert!(frames.is_empty(), "no frames while the last switch lands");
    // Once it has landed (here: timed out), the leg begins from the hand.
    let frames = swipes.motion(&session(), BUTTON, 40, 0, now + SETTLE_TIMEOUT);
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Began]);
}

#[test]
fn a_hold_waiting_on_a_landing_keeps_its_action_until_it_begins() {
    let start = Instant::now();
    let mut swipes = swipes_settling(start);
    let _ = swipes.end(&session(), BUTTON, start);
    let now = start + Duration::from_millis(100);
    assert_eq!(begin_again(&mut swipes, now), None);
    // Released or refused before it begins, the hold falls back on this.
    assert_eq!(
        swipes.unstarted(&session(), BUTTON),
        Some(Action::NextDesktop)
    );
    let frames = swipes.motion(&session(), BUTTON, 40, 0, now + SETTLE_TIMEOUT);
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Began]);
    assert_eq!(swipes.unstarted(&session(), BUTTON), None);
}

#[test]
fn a_hold_that_begins_at_once_has_no_fallback_to_keep() {
    let mut swipes = swipes(PROMPT);
    begin_right(&mut swipes, Instant::now());
    assert_eq!(swipes.unstarted(&session(), BUTTON), None);
}

#[test]
fn canceling_a_settling_hold_still_holds_off_the_next_hold() {
    let start = Instant::now();
    let mut swipes = swipes_settling(start);
    assert!(swipes.cancel(&session(), BUTTON).is_empty());
    assert_eq!(
        begin_again(&mut swipes, start + Duration::from_millis(100)),
        None
    );

    let mut swipes = swipes_settling(start);
    assert!(swipes.cancel_session(&session()).is_empty());
    assert_eq!(
        begin_again(&mut swipes, start + Duration::from_millis(100)),
        None
    );
}

#[test]
fn a_hold_after_the_last_switch_landed_begins_at_once() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let (_, now) = sweep(&mut swipes, start, 45, 10);
    let _ = swipes.end(&session(), BUTTON, now + Duration::from_millis(300));
    assert_eq!(
        begin_again(&mut swipes, now + Duration::from_millis(400)).map(|f| f.phase),
        Some(SpaceSwipePhase::Began)
    );
}

#[test]
fn a_release_while_settling_still_holds_off_the_next_hold() {
    let start = Instant::now();
    let mut swipes = swipes_settling(start);
    assert!(swipes.end(&session(), BUTTON, start).is_empty());
    assert_eq!(
        begin_again(&mut swipes, start + Duration::from_millis(100)),
        None
    );
}

#[test]
fn the_edge_is_read_again_before_holding_there() {
    // The start reads the last desktop: a switch had not been reported yet.
    HAS_NEXT.set(false);
    let mut swipes = swipes(SILENT_UNTIL_LAST);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    // WindowServer catches up mid-drag: the next desktop exists after all.
    HAS_NEXT.set(true);
    let (frames, _) = sweep(&mut swipes, start, 8, 100);
    assert!(
        frames.last().and_then(end_speed).is_some(),
        "the switch finishes instead of holding at a stale edge: {frames:?}"
    );
}

#[test]
fn a_space_change_on_another_display_does_not_end_the_settle() {
    let mut swipes = swipes(ELSEWHERE);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let (frames, now) = sweep(&mut swipes, start, 8, 100);
    assert!(frames.last().and_then(end_speed).is_some(), "{frames:?}");
    // Changes keep being reported, but this display's desktop never moved.
    let (frames, _) = sweep(&mut swipes, now, 10, 100);
    assert!(frames.is_empty(), "still waiting for this display's switch");
}

#[test]
fn canceling_one_hold_springs_back_only_a_transition_in_flight() {
    let start = Instant::now();
    let mut in_flight = swipes(PROMPT);
    begin_right(&mut in_flight, start);
    let _ = in_flight.motion(&session(), BUTTON, 200, 0, start + Duration::from_millis(8));
    let frames = in_flight.cancel(&session(), BUTTON);
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Cancelled]);
    assert!(!in_flight.is_active(&session(), BUTTON));

    let mut settling = swipes_settling(start);
    assert!(
        settling.cancel(&session(), BUTTON).is_empty(),
        "a completed switch has nothing left to spring back"
    );
    assert!(!settling.is_active(&session(), BUTTON));
}

fn swipes_settling(start: Instant) -> SpaceSwipes {
    let mut swipes = swipes(SILENT);
    begin_right(&mut swipes, start);
    let _ = sweep(&mut swipes, start, 8, 100);
    swipes
}

#[test]
fn the_last_desktop_holds_at_the_edge_and_springs_back() {
    let mut swipes = swipes(LAST);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let (frames, now) = sweep(&mut swipes, start, 30, 100);
    assert!(
        frames
            .iter()
            .all(|frame| frame.phase == SpaceSwipePhase::Changed),
        "no switch is asked of a desktop that does not exist"
    );
    assert!((frames.last().expect("frames").progress - 1.0).abs() < 1e-9);
    // Pulling back responds at once from the edge.
    let back = swipes.motion(&session(), BUTTON, -80, 0, now + Duration::from_millis(8));
    assert!((back[0].progress - 0.9).abs() < 1e-9);
    let frames = swipes.end(&session(), BUTTON, now + Duration::from_millis(300));
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Cancelled]);
}

#[test]
fn a_short_slow_drag_springs_back_on_release() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let (_, now) = sweep(&mut swipes, start, 10, 10);
    let frames = swipes.end(&session(), BUTTON, now + Duration::from_millis(300));
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Cancelled]);
    assert!(!swipes.is_active(&session(), BUTTON));
}

#[test]
fn a_drag_past_halfway_finishes_on_release() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let (_, now) = sweep(&mut swipes, start, 45, 10);
    // Released 300 ms after the hand stopped: the switch finishes from rest.
    let frames = swipes.end(&session(), BUTTON, now + Duration::from_millis(300));
    assert_eq!(phases(&frames), vec![SpaceSwipePhase::Ended { speed: 0.0 }]);
    assert!(frames[0].progress >= COMPLETE_AT);
}

#[test]
fn a_short_flick_finishes_where_the_hand_let_go_at_its_speed() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    // 5 × 30 = 150 units (about 0.19 desktop) in 40 ms, released at speed.
    let (_, now) = sweep(&mut swipes, start, 5, 30);
    let frames = swipes.end(&session(), BUTTON, now + Duration::from_millis(4));
    let [frame] = frames.as_slice() else {
        panic!("one Ended: {frames:?}");
    };
    assert!((frame.progress - 150.0 / TRAVEL).abs() < 1e-9, "{frame:?}");
    assert!(
        end_speed(frame).is_some_and(|speed| speed >= FLICK_SPEED),
        "{frame:?}"
    );
}

#[test]
fn dragging_back_swings_to_the_other_neighbour_only_when_it_is_bound() {
    let start = Instant::now();
    let mut both = swipes(PROMPT);
    begin_right(&mut both, start);
    let (frames, _) = sweep(&mut both, start, 10, -20);
    assert!(
        frames.last().expect("frames").progress < 0.0,
        "Left → Previous is bound, so dragging back reaches the previous desktop"
    );

    let mut one_way = swipes(PROMPT);
    one_way
        .begin(
            &session(),
            BUTTON,
            GestureDirection::Right,
            &Action::NextDesktop,
            Some(&Action::MissionControl),
            start,
        )
        .expect("opened");
    let (frames, _) = sweep(&mut one_way, start, 10, -20);
    assert!(
        frames.last().expect("frames").progress.abs() < 1e-9,
        "an unbound reverse stops at the starting Space"
    );
}

#[test]
fn canceling_the_session_springs_back_its_transitions_only() {
    let mut swipes = swipes(PROMPT);
    let start = Instant::now();
    begin_right(&mut swipes, start);
    let other = HidppSessionId::with_epoch("mouse", 2);
    swipes
        .begin(
            &other,
            BUTTON,
            GestureDirection::Left,
            &Action::PreviousDesktop,
            None,
            start,
        )
        .expect("opened");
    assert_eq!(
        phases(&swipes.cancel_session(&session())),
        vec![SpaceSwipePhase::Cancelled]
    );
    assert!(!swipes.is_active(&session(), BUTTON));
    assert!(swipes.is_active(&other, BUTTON));
}
