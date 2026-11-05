//! Absolute-deadline admission for a queued compositor request.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

pub(super) fn is_expired(deadline: Instant) -> bool {
    tokio::time::Instant::now() >= tokio::time::Instant::from_std(deadline)
}

pub(super) async fn before_deadline<F: Future>(deadline: Instant, request: F) -> Option<F::Output> {
    BeforeDeadline {
        deadline: tokio::time::Instant::from_std(deadline),
        timer: Box::pin(tokio::time::sleep_until(tokio::time::Instant::from_std(
            deadline,
        ))),
        request: Box::pin(request),
    }
    .await
}

/// Refuse expiry on every poll. `timeout_at` polls its inner future before its
/// timer, so a request already waiting on its own I/O could dispatch once that
/// I/O completed after the deadline. The timer still wakes a pending request.
struct BeforeDeadline<F> {
    deadline: tokio::time::Instant,
    timer: Pin<Box<tokio::time::Sleep>>,
    request: Pin<Box<F>>,
}

impl<F: Future> Future for BeforeDeadline<F> {
    type Output = Option<F::Output>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if tokio::time::Instant::now() >= this.deadline
            || this.timer.as_mut().poll(context).is_ready()
        {
            return Poll::Ready(None);
        }
        this.request.as_mut().poll(context).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[tokio::test(start_paused = true)]
    async fn queued_request_cannot_poll_a_mutation_after_its_absolute_deadline() {
        let invoked = std::sync::atomic::AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(1);
        let queued = before_deadline(deadline, async {
            invoked.store(true, std::sync::atomic::Ordering::SeqCst);
            42
        });
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(queued.await, None);
        assert!(!invoked.load(std::sync::atomic::Ordering::SeqCst));
        assert!(is_expired(deadline));
    }

    #[tokio::test]
    async fn live_deadline_preserves_the_request_result() {
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(!is_expired(deadline));
        assert_eq!(before_deadline(deadline, async { 42 }).await, Some(42));
    }

    #[tokio::test(start_paused = true)]
    async fn resumed_request_cannot_dispatch_after_its_deadline() {
        use std::{
            pin::pin,
            task::{Context, Poll},
        };

        // A request already pending on its own I/O must not become ready to
        // send merely because that I/O completes after the deadline.
        let invoked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dispatched = invoked.clone();
        let (send, receive) = tokio::sync::oneshot::channel::<()>();
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut queued = pin!(before_deadline(deadline, async move {
            let _ = receive.await;
            dispatched.store(true, std::sync::atomic::Ordering::SeqCst);
            42
        }));
        let waker = futures_util::task::noop_waker();
        let mut context = Context::from_waker(&waker);
        assert!(matches!(queued.as_mut().poll(&mut context), Poll::Pending));
        tokio::time::advance(Duration::from_secs(2)).await;
        send.send(()).unwrap();
        assert_eq!(queued.as_mut().poll(&mut context), Poll::Ready(None));
        assert!(!invoked.load(std::sync::atomic::Ordering::SeqCst));
    }
}
