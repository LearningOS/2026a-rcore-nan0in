//! Mutex (spin-like and blocking(sleep))

use super::deadlock::task_id;
use super::{ResourceState, UPSafeCell};
use crate::task::TaskControlBlock;
use crate::task::{block_current_and_run_next, suspend_current_and_run_next};
use crate::task::{current_task, wakeup_task};
use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
};

fn alive(task: &Arc<TaskControlBlock>) -> bool {
    task.inner_exclusive_access().res.is_some()
}

fn add_owner(state: &mut ResourceState, owner: Option<&Weak<TaskControlBlock>>) {
    if let Some(task) = owner.and_then(Weak::upgrade).filter(alive) {
        state.held.push((task_id(&task), 1));
    }
}

/// Mutex trait
pub trait Mutex: Sync + Send {
    /// Lock the mutex
    fn lock(&self);
    /// Unlock the mutex
    fn unlock(&self);
    /// Snapshot held and waiting resources for deadlock detection.
    fn resources(&self) -> ResourceState;
}

/// Spinlock Mutex struct
pub struct MutexSpin {
    inner: UPSafeCell<SpinState>,
}

struct SpinState {
    owner: Option<Weak<TaskControlBlock>>,
    waiters: BTreeMap<usize, Weak<TaskControlBlock>>,
}

impl MutexSpin {
    /// Create a new spinlock mutex
    pub fn new() -> Self {
        Self {
            inner: unsafe {
                UPSafeCell::new(SpinState {
                    owner: None,
                    waiters: BTreeMap::new(),
                })
            },
        }
    }
}

impl Mutex for MutexSpin {
    fn lock(&self) {
        let task = current_task().unwrap();
        let id = task_id(&task);
        loop {
            let mut inner = self.inner.exclusive_access();
            if inner.owner.is_none() {
                inner.waiters.remove(&id);
                inner.owner = Some(Arc::downgrade(&task));
                return;
            }
            inner
                .waiters
                .entry(id)
                .or_insert_with(|| Arc::downgrade(&task));
            drop(inner);
            suspend_current_and_run_next();
        }
    }

    fn unlock(&self) {
        self.inner.exclusive_access().owner = None;
    }

    fn resources(&self) -> ResourceState {
        let inner = self.inner.exclusive_access();
        let mut state = ResourceState {
            available: usize::from(inner.owner.is_none()),
            ..Default::default()
        };
        add_owner(&mut state, inner.owner.as_ref());
        for (&id, waiter) in &inner.waiters {
            if waiter.upgrade().as_ref().is_some_and(alive) {
                state.waiting.push(id);
            }
        }
        state
    }
}

/// Blocking Mutex struct
pub struct MutexBlocking {
    inner: UPSafeCell<MutexBlockingInner>,
}

pub struct MutexBlockingInner {
    owner: Option<Weak<TaskControlBlock>>,
    wait_queue: VecDeque<Arc<TaskControlBlock>>,
}

impl MutexBlocking {
    /// Create a new blocking mutex
    pub fn new() -> Self {
        trace!("kernel: MutexBlocking::new");
        Self {
            inner: unsafe {
                UPSafeCell::new(MutexBlockingInner {
                    owner: None,
                    wait_queue: VecDeque::new(),
                })
            },
        }
    }
}

impl Mutex for MutexBlocking {
    /// lock the blocking mutex
    fn lock(&self) {
        trace!("kernel: MutexBlocking::lock");
        let mut mutex_inner = self.inner.exclusive_access();
        if mutex_inner.owner.is_some() {
            mutex_inner.wait_queue.push_back(current_task().unwrap());
            drop(mutex_inner);
            block_current_and_run_next();
        } else {
            mutex_inner.owner = Some(Arc::downgrade(&current_task().unwrap()));
        }
    }

    /// unlock the blocking mutex
    fn unlock(&self) {
        trace!("kernel: MutexBlocking::unlock");
        let mut mutex_inner = self.inner.exclusive_access();
        assert!(mutex_inner.owner.is_some());
        if let Some(waking_task) = mutex_inner.wait_queue.pop_front() {
            mutex_inner.owner = Some(Arc::downgrade(&waking_task));
            wakeup_task(waking_task);
        } else {
            mutex_inner.owner = None;
        }
    }
    fn resources(&self) -> ResourceState {
        let inner = self.inner.exclusive_access();
        let mut state = ResourceState {
            available: usize::from(inner.owner.is_none()),
            ..Default::default()
        };
        add_owner(&mut state, inner.owner.as_ref());
        state.waiting.extend(
            inner
                .wait_queue
                .iter()
                .filter(|task| alive(task))
                .map(task_id),
        );
        state
    }
}
