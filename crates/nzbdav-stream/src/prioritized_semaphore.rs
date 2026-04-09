//! `PrioritizedSemaphore` — a two-tier permit system where high-priority
//! acquirers can use all permits while low-priority acquirers are limited
//! to a subset.

use std::sync::Arc;

use tokio::sync::Semaphore;

/// A semaphore with two priority tiers.
///
/// - **High priority** can acquire from the full pool of `total_permits`.
/// - **Low priority** can only acquire from `total_permits - high_priority_reserved`,
///   ensuring that `high_priority_reserved` permits are always available for
///   high-priority work.
pub struct PrioritizedSemaphore {
    /// Semaphore shared by both tiers. Total capacity = `total_permits`.
    shared: Arc<Semaphore>,
    /// Additional gate for low priority: capacity = `total - reserved`.
    low_gate: Arc<Semaphore>,
}

/// RAII permit that releases back to the appropriate semaphores on drop.
pub struct PrioritizedPermit {
    _shared_permit: tokio::sync::OwnedSemaphorePermit,
    _low_permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl PrioritizedSemaphore {
    /// Create a new semaphore.
    ///
    /// * `total_permits` — maximum concurrent permits across both tiers.
    /// * `high_priority_reserved` — how many permits are reserved exclusively
    ///   for high-priority acquirers. Must be `<= total_permits`.
    ///
    /// # Panics
    ///
    /// Panics if `high_priority_reserved > total_permits`.
    pub fn new(total_permits: usize, high_priority_reserved: usize) -> Self {
        assert!(
            high_priority_reserved <= total_permits,
            "reserved ({high_priority_reserved}) must be <= total ({total_permits})"
        );

        let low_capacity = total_permits - high_priority_reserved;
        Self {
            shared: Arc::new(Semaphore::new(total_permits)),
            low_gate: Arc::new(Semaphore::new(low_capacity)),
        }
    }

    /// Acquire a high-priority permit. Waits only on the shared semaphore,
    /// so it can use the full pool including the reserved slots.
    pub async fn acquire_high(&self) -> PrioritizedPermit {
        let shared_permit = Arc::clone(&self.shared)
            .acquire_owned()
            .await
            .expect("shared semaphore closed unexpectedly");

        PrioritizedPermit {
            _shared_permit: shared_permit,
            _low_permit: None,
        }
    }

    /// Acquire a low-priority permit. Must acquire from both the low gate
    /// (which limits low-priority concurrency) and the shared semaphore.
    pub async fn acquire_low(&self) -> PrioritizedPermit {
        // Acquire the low gate first to enforce the low-priority cap.
        let low_permit = Arc::clone(&self.low_gate)
            .acquire_owned()
            .await
            .expect("low gate semaphore closed unexpectedly");

        // Then acquire from the shared pool.
        let shared_permit = Arc::clone(&self.shared)
            .acquire_owned()
            .await
            .expect("shared semaphore closed unexpectedly");

        PrioritizedPermit {
            _shared_permit: shared_permit,
            _low_permit: Some(low_permit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn test_new_creates_permits() {
        let sem = PrioritizedSemaphore::new(10, 3);
        // High priority should be able to acquire (uses shared pool of 10)
        let _h = sem.acquire_high().await;
        // Low priority should also work (shared=10, low_gate=7)
        let _l = sem.acquire_low().await;
    }

    #[tokio::test]
    async fn test_high_priority_gets_reserved() {
        let sem = PrioritizedSemaphore::new(2, 2);
        // High priority can acquire both permits
        let _h1 = sem.acquire_high().await;
        let _h2 = sem.acquire_high().await;

        // Low priority should block because low_gate capacity is 0
        let sem2 = PrioritizedSemaphore::new(2, 2);
        let result = timeout(Duration::from_millis(100), sem2.acquire_low()).await;
        assert!(
            result.is_err(),
            "acquire_low should block when all permits are reserved"
        );
    }

    #[tokio::test]
    async fn test_low_priority_uses_shared() {
        let sem = PrioritizedSemaphore::new(4, 1);
        // low_gate capacity = 3, so 3 low acquires should succeed
        let _l1 = sem.acquire_low().await;
        let _l2 = sem.acquire_low().await;
        let _l3 = sem.acquire_low().await;

        // 4th low acquire should block (low_gate exhausted)
        let result = timeout(Duration::from_millis(100), sem.acquire_low()).await;
        assert!(result.is_err(), "4th acquire_low should block");
    }

    #[tokio::test]
    async fn test_permit_drop_releases() {
        let sem = PrioritizedSemaphore::new(1, 1);
        let h = sem.acquire_high().await;
        drop(h);
        // After drop, we should be able to acquire again
        let _h2 = sem.acquire_high().await;
    }

    #[tokio::test]
    async fn test_concurrent_high_low() {
        let sem = Arc::new(PrioritizedSemaphore::new(2, 1));
        // low_gate capacity = 1, shared = 2

        // Exhaust all shared permits with high priority
        let h1 = sem.acquire_high().await;
        let h2 = sem.acquire_high().await;

        // Low should be blocked (shared exhausted)
        let sem_clone = Arc::clone(&sem);
        let handle = tokio::spawn(async move { sem_clone.acquire_low().await });

        // Give the spawned task a chance to start waiting
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!handle.is_finished(), "low acquire should be blocked");

        // Release one high permit — low should now unblock
        drop(h1);
        let result = timeout(Duration::from_millis(200), handle).await;
        assert!(
            result.is_ok(),
            "low acquire should unblock after high permit released"
        );

        drop(h2);
    }
}
