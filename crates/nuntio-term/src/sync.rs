//! Locking that ignores poisoning.
//!
//! A panic on one pane thread must not take the UI thread down with it: the
//! guarded state (queues, sizes, the palette) stays consistent between its
//! writes, so a poisoned lock is simply taken over.

use std::sync::{
    LockResult, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
};

/// The value of a lock operation, poisoned or not.
pub(crate) fn unpoisoned<T>(result: LockResult<T>) -> T {
    result.unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    unpoisoned(mutex.lock())
}

pub(crate) fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    unpoisoned(lock.read())
}

pub(crate) fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    unpoisoned(lock.write())
}
