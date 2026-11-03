//! Large-stack execution for the recursive pipeline stages.
//!
//! The evaluator and the kernel resolver are recursive tree walks. Their
//! depth guards ([`MAX_CALL_DEPTH`](crate::eval::MAX_CALL_DEPTH),
//! [`MAX_EXPR_DEPTH`](crate::eval::MAX_EXPR_DEPTH),
//! [`MAX_INST_DEPTH`](crate::eval::MAX_INST_DEPTH), the kernel's resolve cap)
//! bound the recursion, but the native stack needed to *reach* those limits
//! under debug frame sizes is far more than a default 2 MiB thread stack —
//! `module m(n) if (n > 0) translate([1,0,0]) m(n-1); m(510);` overflowed a
//! 2 MiB `spawn_blocking` thread (a process abort, not an error). Rather than
//! trusting every embedder to spawn a big-stack thread, the public pipeline
//! entry points ([`Evaluator::eval_program`](crate::eval::Evaluator::eval_program)
//! and the kernel's `resolve`/`resolve_root`) run their recursion through
//! [`with_large_stack`] internally, so callers need no change.

use std::cell::Cell;

/// Native stack size of the dedicated pipeline thread: 256 MiB.
///
/// Sized well above what the deepest legal recursion needs (every depth guard
/// tripping at once under unoptimised debug frames) so the guards — not the
/// stack — are always what stops a pathological program. The reservation is
/// virtual address space only: pages are committed as the stack actually
/// grows, so an ordinary compile touches a few hundred KiB of it.
pub const PIPELINE_STACK_BYTES: usize = 256 * 1024 * 1024;

thread_local! {
    /// Set while the current thread is a [`with_large_stack`] worker, so
    /// nested entry points (eval → resolve, or a caller that already wrapped
    /// its whole pipeline) run inline instead of spawning again.
    static ON_LARGE_STACK: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` on a dedicated thread with a [`PIPELINE_STACK_BYTES`] stack and
/// return its result, blocking the caller until it finishes.
///
/// Runs `f` inline when already on such a thread, or when the OS refuses to
/// spawn one (degrading to the caller's stack rather than failing outright).
/// A panic inside `f` is re-raised on the calling thread unchanged.
pub fn with_large_stack<T, F>(f: F) -> T
where
    T: Send,
    F: FnOnce() -> T + Send,
{
    if ON_LARGE_STACK.with(Cell::get) {
        return f();
    }
    with_stack_size(PIPELINE_STACK_BYTES, f)
}

/// Run `f` on a dedicated pipeline thread with a `stack_bytes` native stack,
/// even when already on one. Entry points reached from `f` treat the thread
/// as their large stack and run inline, so this pins the stack budget a whole
/// pipeline runs under — the depth-guard sizing tests use it to prove the
/// guards trip well before [`PIPELINE_STACK_BYTES`] is exhausted.
///
/// Falls back to the caller's stack when the OS refuses to spawn the thread,
/// and re-raises a panic inside `f` on the calling thread, like
/// [`with_large_stack`].
pub fn with_stack_size<T, F>(stack_bytes: usize, f: F) -> T
where
    T: Send,
    F: FnOnce() -> T + Send,
{
    // `f` must be movable into the worker yet recoverable if the spawn fails;
    // park it in an Option the worker takes from.
    let slot = std::sync::Mutex::new(Some(f));
    let outcome = std::thread::scope(|scope| {
        let spawned = std::thread::Builder::new()
            .name("morphe-pipeline".to_string())
            .stack_size(stack_bytes)
            .spawn_scoped(scope, || {
                ON_LARGE_STACK.with(|flag| flag.set(true));
                let f = slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                    .expect("pipeline closure taken once");
                f()
            });
        spawned.map(|handle| handle.join())
    });
    match outcome {
        Ok(Ok(value)) => value,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        // Spawn failed (resource exhaustion): run on the caller's stack.
        Err(_) => {
            let f = slot
                .into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .expect("closure untouched when the spawn failed");
            f()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recurse `n` levels with a deliberately fat frame.
    fn deep(n: usize) -> usize {
        let pad = std::hint::black_box([0u8; 4096]);
        if n == 0 {
            usize::from(pad[0])
        } else {
            deep(n - 1) + 1 + usize::from(std::hint::black_box(&pad)[1])
        }
    }

    #[test]
    fn runs_recursion_too_deep_for_a_small_stack() {
        // ~20k frames × 4 KiB ≈ 80 MiB: overflows a 2 MiB caller thread but
        // fits the pipeline stack. Invoked from a 2 MiB thread to prove the
        // helper (not the test harness's stack) is what provides the room.
        let got = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| with_large_stack(|| deep(20_000)))
            .unwrap()
            .join()
            .expect("must not overflow");
        assert_eq!(got, 20_000);
    }

    #[test]
    fn nested_calls_run_inline() {
        let outer = with_large_stack(|| {
            let id = std::thread::current().id();
            with_large_stack(move || std::thread::current().id() == id)
        });
        assert!(outer, "a nested call must reuse the worker thread");
    }

    #[test]
    fn panics_propagate_to_the_caller() {
        let r = std::panic::catch_unwind(|| with_large_stack(|| panic!("boom")));
        assert!(r.is_err());
    }
}
