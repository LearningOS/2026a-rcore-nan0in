//! Semaphore

use super::deadlock::task_id;
use super::{ResourceState, UPSafeCell};
use crate::task::{block_current_and_run_next, current_task, wakeup_task, TaskControlBlock};
use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
};

/// semaphore structure
pub struct Semaphore {
    /// semaphore inner
    pub inner: UPSafeCell<SemaphoreInner>,
}

pub struct SemaphoreInner {
    pub count: isize,
    pub wait_queue: VecDeque<Arc<TaskControlBlock>>,
    held: BTreeMap<usize, (Weak<TaskControlBlock>, usize)>,
}

impl SemaphoreInner {
    fn grant(&mut self, task: &Arc<TaskControlBlock>) {
        self.held
            .entry(task_id(task))
            .or_insert_with(|| (Arc::downgrade(task), 0))
            .1 += 1;
    }

    fn release(&mut self, task: &Arc<TaskControlBlock>) {
        let id = task_id(task);
        if let Some((_, count)) = self.held.get_mut(&id) {
            *count -= 1;
            if *count == 0 {
                self.held.remove(&id);
            }
        }
    }
}

impl Semaphore {
    /// Create a new semaphore
    pub fn new(res_count: usize) -> Self {
        trace!("kernel: Semaphore::new");
        Self {
            inner: unsafe {
                UPSafeCell::new(SemaphoreInner {
                    count: res_count as isize,
                    wait_queue: VecDeque::new(),
                    held: BTreeMap::new(),
                })
            },
        }
    }

    /// up operation of semaphore
    pub fn up(&self) {
        trace!("kernel: Semaphore::up");
        let mut inner = self.inner.exclusive_access();
        inner.release(&current_task().unwrap());
        inner.count = inner.count.saturating_add(1);
        if inner.count <= 0 {
            if let Some(task) = inner.wait_queue.pop_front() {
                inner.grant(&task);
                wakeup_task(task);
            }
        }
    }

    /// down operation of semaphore
    pub fn down(&self) {
        trace!("kernel: Semaphore::down");
        let mut inner = self.inner.exclusive_access();
        inner.count -= 1;
        if inner.count < 0 {
            inner.wait_queue.push_back(current_task().unwrap());
            drop(inner);
            block_current_and_run_next();
        } else {
            inner.grant(&current_task().unwrap());
        }
    }
    /// Snapshot available tokens, holders and pending requests.
    pub fn resources(&self) -> ResourceState {
        let mut inner = self.inner.exclusive_access();
        inner.held.retain(|_, (task, _)| {
            task.upgrade()
                .is_some_and(|task| task.inner_exclusive_access().res.is_some())
        });
        ResourceState {
            available: inner.count.max(0) as usize,
            held: inner
                .held
                .iter()
                .map(|(&id, (_, count))| (id, *count))
                .collect(),
            waiting: inner
                .wait_queue
                .iter()
                .filter(|task| task.inner_exclusive_access().res.is_some())
                .map(task_id)
                .collect(),
        }
    }
}
