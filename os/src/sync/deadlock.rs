//! Resource-reduction deadlock check, run on a snapshot before blocking.
// Resource reduction using available, allocation and need follows the Lab 5 guide:
// https://learningos.cn/rCore-Tutorial-Guide/chapter8/5exercise.html
use crate::task::TaskControlBlock;
use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

/// Snapshot of one mutex or semaphore's available, held and requested units.
#[derive(Default)]
pub struct ResourceState {
    pub(crate) available: usize,
    pub(crate) held: Vec<(usize, usize)>,
    pub(crate) waiting: Vec<usize>,
}

pub(super) fn task_id(task: &Arc<TaskControlBlock>) -> usize {
    Arc::as_ptr(task) as usize
}

/// Add one proposed request and see whether all threads can make progress.
pub(crate) fn would_deadlock(
    mut resources: Vec<ResourceState>,
    requested: usize,
    caller: &Arc<TaskControlBlock>,
) -> bool {
    #[derive(Default)]
    struct Thread {
        needs: Vec<usize>,
        holds: Vec<(usize, usize)>,
    }
    let mut threads = BTreeMap::<usize, Thread>::new();
    resources[requested].waiting.push(task_id(caller));
    let mut work = Vec::new();
    for (id, resource) in resources.into_iter().enumerate() {
        work.push(resource.available);
        for (task, count) in resource.held {
            threads.entry(task).or_default().holds.push((id, count));
        }
        for task in resource.waiting {
            threads.entry(task).or_default().needs.push(id);
        }
    }
    while let Some((&task, _)) = threads
        .iter()
        .find(|(_, thread)| thread.needs.iter().all(|&id| work[id] != 0))
    {
        for (id, count) in threads.remove(&task).unwrap().holds {
            work[id] = work[id].saturating_add(count);
        }
    }
    !threads.is_empty()
}
