//! Regression tests for the desktop AI-turn lifecycle and the off-main worker
//! (review-hardening wave).
//!
//! - The in-flight turn slot is released when a turn ends, so the SECOND turn
//!   of a session is accepted (previously every turn after the first failed
//!   with "an AI turn is already in flight": the slot was never cleared).
//! - A cancelled turn keeps blocking until it has actually finished.
//! - Rejecting an edit mid-turn (when the backend buffer does not hold the
//!   agent's edit yet) records the id and never errors.
//! - Deep (legal) recursion runs on the large-stack worker; panics become
//!   errors instead of unwinding into the command.

use morphe_desktop_lib::{reject_edit_inner, run_on_big_stack, AppState};

#[test]
fn second_turn_is_accepted_after_first_turn_finishes() {
    let state = AppState::new_in_memory();
    let (_cancel, guard) = state.try_begin_turn().expect("first turn starts");
    assert!(state.turn_in_flight());
    // The turn's bridge task ends → its guard drops → the slot is released.
    drop(guard);
    assert!(
        !state.turn_in_flight(),
        "a finished turn must release the slot"
    );
    let (_cancel2, _guard2) = state
        .try_begin_turn()
        .expect("the second turn of a session must be accepted");
}

#[test]
fn concurrent_turn_is_rejected_while_one_is_running() {
    let state = AppState::new_in_memory();
    let (_cancel, _guard) = state.try_begin_turn().expect("first turn starts");
    let err = state
        .try_begin_turn()
        .err()
        .expect("overlapping turn must be rejected");
    assert!(err.contains("already in flight"), "got: {err}");
}

#[test]
fn cancelled_turn_blocks_until_it_has_actually_stopped() {
    let state = AppState::new_in_memory();
    let (cancel, guard) = state.try_begin_turn().expect("first turn starts");
    cancel.cancel();
    let err = state
        .try_begin_turn()
        .err()
        .expect("a cancelled-but-still-draining turn must block a new one");
    assert!(err.contains("still stopping"), "got: {err}");
    drop(guard);
    assert!(
        state.try_begin_turn().is_ok(),
        "after it stops, a new turn starts"
    );
}

#[test]
fn turn_slot_is_released_even_if_the_turn_task_panics() {
    let state = AppState::new_in_memory();
    let (_cancel, guard) = state.try_begin_turn().expect("first turn starts");
    let result = std::thread::spawn(move || {
        let _guard = guard;
        panic!("simulated bridge panic");
    })
    .join();
    assert!(result.is_err());
    assert!(!state.turn_in_flight(), "unwinding must release the slot");
}

#[test]
fn reject_mid_turn_records_id_and_does_not_error() {
    let state = AppState::new_in_memory();
    // Mid-turn the backend buffer does not contain the agent's edit yet.
    state.set_source(String::new());
    let out = reject_edit_inner("main.scad", "cube(1);", "cube(2);", "t1".into(), &state);
    assert_eq!(
        out, "",
        "nothing to revert yet — buffer unchanged, no error"
    );
}

#[test]
fn reject_after_turn_reverts_the_backend_buffer() {
    let state = AppState::new_in_memory();
    state.set_source("cube(2);\n".into());
    let out = reject_edit_inner("main.scad", "cube(1);", "cube(2);", "t1".into(), &state);
    assert_eq!(out, "cube(1);\n");
    assert_eq!(state.source(), "cube(1);\n");
}

#[test]
fn big_stack_worker_survives_deep_recursion() {
    fn depth(n: u64) -> u64 {
        // Force a sizeable frame so the recursion would blow a 2 MiB stack.
        let pad = std::hint::black_box([n; 64]);
        if n == 0 {
            0
        } else {
            1 + depth(n - 1) + (pad[63] - n)
        }
    }
    let r = run_on_big_stack(|| depth(50_000)).expect("no overflow on the worker");
    assert_eq!(r, 50_000);
}

#[test]
fn big_stack_worker_maps_panics_to_errors() {
    let err = run_on_big_stack(|| -> u32 { panic!("capacity overflow") })
        .expect_err("a panic must surface as Err");
    assert!(err.contains("capacity overflow"), "got: {err}");
}
