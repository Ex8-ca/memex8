//! Maintenance read gate: shared/exclusive locking plus a re-entrant read scope.
//!
//! Evidence erasure and other maintenance writers take the exclusive side and
//! wait for readers. Readers take the shared side. The shared side alone is not
//! enough at a transport boundary: once an engine read returns, its guard is
//! dropped and an erasure can commit before the caller has built the response.
//! `read_gated` closes that window by holding the shared lock across result
//! construction and serialization.
//!
//! The lock must not be acquired twice by the same task: `tokio::sync::RwLock`
//! is fair, so a nested `read()` queues behind a waiting writer that the caller
//! itself is blocking — a permanent deadlock. `read_gated` therefore records its
//! scope in a task-local, and `read()` returns [`ReadPermit::Nested`] inside it
//! instead of taking the lock again. Spawned tasks do not inherit task-locals,
//! so they still acquire the lock normally.
use std::future::Future;

use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

tokio::task_local! {
    /// Non-zero while the current task runs inside [`MaintenanceGate::read_gated`].
    static GATE_READ_DEPTH: u32;
}

/// Proof that the caller may read without acquiring the shared lock a second time.
#[derive(Debug)]
pub enum ReadPermit<'a> {
    /// This permit owns the shared lock until it is dropped.
    Held(RwLockReadGuard<'a, ()>),
    /// An enclosing [`MaintenanceGate::read_gated`] scope already holds the lock.
    Nested,
}

/// Shared gate between maintenance writers (evidence erasure, certification,
/// content replacement) and readers.
#[derive(Debug, Default)]
pub struct MaintenanceGate {
    lock: RwLock<()>,
}

impl MaintenanceGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wait for exclusive access. Writers must never run inside `read_gated`.
    pub async fn write(&self) -> RwLockWriteGuard<'_, ()> {
        self.lock.write().await
    }

    /// Non-blocking exclusive probe; used by tests to prove a read scope is held.
    pub fn try_write(&self) -> Result<RwLockWriteGuard<'_, ()>, tokio::sync::TryLockError> {
        self.lock.try_write()
    }

    /// Acquire the shared side unless this task is already inside `read_gated`.
    pub async fn read(&self) -> ReadPermit<'_> {
        if Self::read_scope_is_active() {
            return ReadPermit::Nested;
        }
        ReadPermit::Held(self.lock.read().await)
    }

    /// Run `future` to completion while holding the shared lock, so everything
    /// the future builds (a tool result, its serialized bytes) is constructed
    /// before a waiting erasure can commit. Re-entrant for nested reads in the
    /// same task; spawned tasks are not covered and must acquire on their own.
    pub async fn read_gated<F: Future>(&self, future: F) -> F::Output {
        if Self::read_scope_is_active() {
            return future.await;
        }
        let _held = self.lock.read().await;
        GATE_READ_DEPTH.scope(1u32, future).await
    }

    fn read_scope_is_active() -> bool {
        GATE_READ_DEPTH
            .try_with(|depth| *depth > 0)
            .unwrap_or(false)
    }
}
