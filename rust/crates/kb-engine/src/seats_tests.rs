//! The seat gate: counting, priority order, and the paths that must give a
//! seat back.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

/// A priority for a job of a given age.
fn priority(created_at: i64) -> SeatPriority {
    SeatPriority::new(Some(created_at), format!("job_{created_at}"))
}

/// Wait until `condition` holds, or give up after a second.
async fn wait_for<F: Fn() -> bool>(condition: F) -> bool {
    for _ in 0..200 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    false
}

#[tokio::test]
async fn the_gate_never_hands_out_more_than_its_capacity() {
    let seats = Seats::new(2);
    let first = seats.acquire(priority(1)).await;
    let second = seats.acquire(priority(2)).await;
    assert_eq!(seats.taken(), 2);

    // The third seat waits for one of the two to come back.
    let waiting = seats.clone();
    let task = tokio::spawn(async move { waiting.acquire(priority(3)).await });
    assert!(wait_for(|| seats.waiting() == 1).await);
    assert_eq!(seats.taken(), 2, "a waiter never holds a seat");

    drop(first);
    let third = task.await.unwrap();
    assert_eq!(seats.taken(), 2);
    drop(second);
    drop(third);
    assert_eq!(seats.taken(), 0, "every seat came back");
}

#[tokio::test]
async fn the_oldest_waiter_gets_the_next_seat() {
    let seats = Seats::new(1);
    let held = seats.acquire(priority(0)).await;

    let order = Arc::new(Mutex::new(Vec::new()));
    // Three waiters, queued in the order 3, 1, 2 — the queue must not care.
    for age in [3, 1, 2] {
        let seats = seats.clone();
        let order = Arc::clone(&order);
        tokio::spawn(async move {
            let _guard = seats.acquire(priority(age)).await;
            order.lock().unwrap().push(age);
            // Hold the seat, so the next waiter cannot overtake this one.
            tokio::time::sleep(Duration::from_millis(20)).await;
        });
    }
    assert!(wait_for(|| seats.waiting() == 3).await);

    drop(held);
    assert!(wait_for(|| order.lock().unwrap().len() == 3).await);
    assert_eq!(*order.lock().unwrap(), vec![1, 2, 3]);
}

#[tokio::test]
async fn a_waiter_with_an_unknown_age_is_served_last() {
    let seats = Seats::new(1);
    let held = seats.acquire(priority(0)).await;

    let order = Arc::new(Mutex::new(Vec::new()));
    for (label, age) in [("unknown", None), ("known", Some(9))] {
        let seats = seats.clone();
        let order = Arc::clone(&order);
        tokio::spawn(async move {
            let _guard = seats.acquire(SeatPriority::new(age, label)).await;
            order.lock().unwrap().push(label);
            tokio::time::sleep(Duration::from_millis(20)).await;
        });
    }
    assert!(wait_for(|| seats.waiting() == 2).await);

    drop(held);
    assert!(wait_for(|| order.lock().unwrap().len() == 2).await);
    assert_eq!(*order.lock().unwrap(), vec!["known", "unknown"]);
}

#[tokio::test]
async fn a_guard_dropped_by_a_panic_releases_its_seat() {
    let seats = Seats::new(1);
    let panicking = seats.clone();
    let task = tokio::spawn(async move {
        let _guard = panicking.acquire(priority(1)).await;
        panic!("the stage blew up");
    });
    assert!(task.await.is_err(), "the task panicked");
    assert_eq!(seats.taken(), 0, "the panic gave the seat back");

    // And the seat is usable again.
    let guard = tokio::time::timeout(Duration::from_millis(500), seats.acquire(priority(2))).await;
    assert!(guard.is_ok(), "the seat was not released by the panic");
}

#[tokio::test]
async fn a_cancelled_acquire_does_not_leak_a_seat() {
    let seats = Seats::new(1);
    let held = seats.acquire(priority(0)).await;

    // A waiter that is cancelled while it waits.
    let waiting = seats.clone();
    let task = tokio::spawn(async move { waiting.acquire(priority(1)).await });
    assert!(wait_for(|| seats.waiting() == 1).await);
    task.abort();
    let _ = task.await;

    // The holder gives its seat back: the cancelled waiter must not have
    // taken it, so the seat is free again.
    drop(held);
    let next = tokio::time::timeout(Duration::from_millis(500), seats.acquire(priority(2))).await;
    assert!(next.is_ok(), "the cancelled waiter leaked the seat");
    assert_eq!(seats.taken(), 1);
}

#[tokio::test]
async fn a_cancelled_acquire_does_not_steal_a_seat_from_a_live_waiter() {
    let seats = Seats::new(1);
    let held = seats.acquire(priority(0)).await;

    // The first waiter is cancelled, the second one waits for real.
    let cancelled = seats.clone();
    let first = tokio::spawn(async move { cancelled.acquire(priority(1)).await });
    assert!(wait_for(|| seats.waiting() == 1).await);
    first.abort();
    let _ = first.await;

    let live = seats.clone();
    let second = tokio::spawn(async move { live.acquire(priority(2)).await });
    assert!(wait_for(|| seats.waiting() == 1).await);

    drop(held);
    let guard = tokio::time::timeout(Duration::from_millis(500), second).await;
    assert!(guard.is_ok(), "the live waiter never got the seat");
}

#[tokio::test]
async fn a_seat_that_is_handed_over_and_then_cancelled_comes_back() {
    // The race that matters: the seat is handed to a waiter that is dropped
    // before it wakes up. The seat must not disappear.
    let seats = Seats::new(1);
    let held = seats.acquire(priority(0)).await;

    let waiting = seats.clone();
    let task = tokio::spawn(async move { waiting.acquire(priority(1)).await });
    assert!(wait_for(|| seats.waiting() == 1).await);

    // Give the seat over and drop the waiter at the same time.
    drop(held);
    task.abort();
    let _ = task.await;

    // Give the gate a moment to notice the dead waiter, then check the seat
    // is back in the pool.
    assert!(wait_for(|| seats.taken() == 0).await, "the seat was leaked");
    let guard = tokio::time::timeout(Duration::from_millis(500), seats.acquire(priority(2))).await;
    assert!(guard.is_ok());
}

#[tokio::test]
async fn many_waiters_are_served_one_at_a_time_and_all_of_them_come_back() {
    let seats = Seats::new(3);
    let holders = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for age in 0..20 {
        let seats = seats.clone();
        let holders = Arc::clone(&holders);
        let peak = Arc::clone(&peak);
        tasks.push(tokio::spawn(async move {
            let _guard = seats.acquire(priority(age)).await;
            let now = holders.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            holders.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert!(peak.load(Ordering::SeqCst) <= 3, "more holders than seats");
    assert_eq!(seats.taken(), 0, "every seat came back");
}

#[tokio::test]
async fn the_stage_seats_are_three_gates() {
    let seats = StageSeats::new(1, 2, 3);
    assert_eq!(seats.extract.capacity(), 1);
    assert_eq!(seats.embed.capacity(), 2);
    assert_eq!(seats.store.capacity(), 3);
    // The default capacity is the one the server uses.
    let default = SeatCapacity::default();
    assert_eq!((default.extract, default.embed, default.store), (2, 4, 2));
}

#[test]
fn the_priority_order_is_by_age_then_by_id() {
    let mut ages = [
        SeatPriority::new(Some(3), "b"),
        SeatPriority::new(Some(1), "z"),
        SeatPriority::new(Some(2), "a"),
        SeatPriority::new(Some(1), "a"),
    ];
    ages.sort();
    let order: Vec<(Option<i64>, &str)> = ages
        .iter()
        .map(|p| (p.created_at, p.job_id.as_str()))
        .collect();
    assert_eq!(
        order,
        vec![
            (Some(1), "a"),
            (Some(1), "z"),
            (Some(2), "a"),
            (Some(3), "b"),
        ]
    );
}
