#![allow(dead_code)]

//! Cross-thread signal-update helpers.
//!
//! Floem 0.2's `create_ext_action` (in `floem::ext_event`) is `FnOnce`
//! (single-shot) — fine for one-time dialog returns (Ctrl-O, Ctrl-S
//! Save-As, Find/Replace commit) but unusable for a periodic updater
//! like the FPS / RAM sampler that fires every 500 ms forever.
//!
//! This module wraps the `Trigger` + `mpsc::Receiver` + `create_effect`
//! drain pattern into a reusable function. The background thread pushes
//! values onto a `std::sync::mpsc` channel and pokes the GUI event loop
//! via `register_ext_trigger`; a reactive effect on the GUI thread
//! tracks the trigger, drains the channel, and applies the values to
//! whatever `RwSignal`s the caller cares about. Only the GUI thread
//! ever touches a `RwSignal`, which is exactly what Floem 0.2 wants.
//!
//! ## Floem 0.2 API surprises (vs. the round-5 design sketch)
//!
//! The original Sentinel issue #1 fix-design mentioned a hypothetical
//! `ExtSendTrigger::new()` / `.notify()` / `.track()` triple from
//! `floem::ext_event`. The **actual** 0.2 surface is:
//!
//! * `floem::reactive::Trigger` — `Copy + Clone`, with
//!   `new()` / `track()` / `notify()`. `notify()` simply does
//!   `RwSignal::set(())` on its inner unit signal — **same thread-safety
//!   caveat as any other `RwSignal::set`**, so we MUST NOT call
//!   `trigger.notify()` from the background thread.
//! * `floem::ext_event::register_ext_trigger(trigger: Trigger)` — the
//!   thread-safe path. It pushes the trigger onto the global
//!   `EXT_EVENT_HANDLER` queue (a `parking_lot::Mutex<VecDeque<Trigger>>`)
//!   and wakes the event loop via `proxy.send_event(UserEvent::Idle)`.
//!   The runtime drains the queue and calls `.notify()` on each trigger
//!   from the GUI thread, which is what kicks our `create_effect` back
//!   to life.
//! * `floem::ext_event::create_ext_action` is unchanged from the design
//!   sketch (one-shot `FnOnce`) and stays re-exported here for symmetry.

use std::sync::mpsc::{channel, Receiver, Sender};

use floem::ext_event::register_ext_trigger;
use floem::reactive::{Scope, Trigger};

/// Spawn a background thread that periodically computes a value of type
/// `T` and publishes it onto the GUI thread, where `on_main` is invoked
/// once per produced item.
///
/// * `cx` — reactive scope owning the GUI-side effect. Pass
///   `Scope::current()` from inside a `Application::new().window(...)`
///   builder closure.
/// * `interval` — sleep budget between successive `produce()` calls on
///   the worker thread.
/// * `produce` — runs on the worker thread; returns the value to
///   publish.
/// * `on_main` — runs on the GUI thread, once per item drained from the
///   channel. Safe to `.set` any `RwSignal` here.
///
/// Returns the channel `Sender` so additional threads can also push
/// items into the same pipeline if useful.
///
/// The worker thread exits cleanly when the channel `Sender` is dropped
/// (i.e. when the reactive scope is disposed and the effect / receiver
/// go away).
pub fn spawn_periodic<T: Send + 'static>(
    cx: Scope,
    interval: std::time::Duration,
    mut produce: impl FnMut() -> T + Send + 'static,
    on_main: impl Fn(T) + 'static,
) -> Sender<T> {
    let trigger = Trigger::new();
    let (tx, rx): (Sender<T>, Receiver<T>) = channel();
    let tx_thread = tx.clone();

    // Background producer thread. Sleeps `interval`, computes a value,
    // pushes it through the channel, then asks the event loop to wake
    // up the GUI-side effect via `register_ext_trigger`. We must NOT
    // call `trigger.notify()` here — that does `RwSignal::set` and is
    // exactly the cross-thread footgun Sentinel #1 reported.
    std::thread::spawn(move || loop {
        std::thread::sleep(interval);
        let v = produce();
        if tx_thread.send(v).is_err() {
            // Receiver side dropped (scope disposed) — exit cleanly.
            return;
        }
        register_ext_trigger(trigger);
    });

    // GUI-side effect: when the trigger fires, drain everything the
    // worker has queued and hand each item to `on_main`. `try_recv` so
    // we never block the UI thread; the loop terminates when the queue
    // is empty (and resumes on the next fire of the trigger).
    cx.create_effect(move |_| {
        trigger.track();
        while let Ok(v) = rx.try_recv() {
            on_main(v);
        }
    });

    tx
}

/// One-shot variant — re-exported here for symmetry so callers have a
/// single import for both patterns. Use this for "do X once on the GUI
/// thread when a worker finishes" (file dialog return, save completion,
/// etc.).
pub use floem::ext_event::create_ext_action;

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{with_scope, Scope};

    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    #[test]
    fn trigger_basic_smoke() {
        run(|| {
            let trigger = Trigger::new();
            trigger.track();
            trigger.notify();
        });
    }

    #[test]
    fn trigger_clone_eq() {
        run(|| {
            let trigger1 = Trigger::new();
            let trigger2 = trigger1;
            trigger2.track();
        });
    }

    #[test]
    fn create_ext_action_smoke() {
        run(|| {
            let _action = create_ext_action(Scope::current(), |v: i32| {
                assert_eq!(v, 42);
            });
        });
    }

    #[test]
    fn spawn_periodic_smoke() {
        run(|| {
            let sender = spawn_periodic(
                Scope::current(),
                std::time::Duration::from_millis(10),
                || 100,
                |v| {
                    assert_eq!(v, 100);
                },
            );
            sender.send(42).unwrap();
        });
    }

    #[test]
    fn ext_action_is_callable() {
        run(|| {
            let action = create_ext_action(Scope::current(), move |val: i32| {
                assert_eq!(val, 123);
            });
            action(123);
        });
    }

    #[test]
    fn trigger_can_notify() {
        run(|| {
            let t = Trigger::new();
            t.notify();
        });
    }
}
