//! Process management syscalls
//!
use crate::mm::{PTEFlags, PageTable, VirtAddr};
use crate::timer::get_time_us;
use alloc::sync::Arc;

use crate::{
    fs::{open_file, OpenFlags},
    mm::{translated_refmut, translated_str},
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next,
    },
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
    trace!("kernel: sys_getpid pid:{}", current_task().unwrap().pid.0);
    current_task().unwrap().pid.0 as isize
}

pub fn sys_fork() -> isize {
    trace!("kernel:pid[{}] sys_fork", current_task().unwrap().pid.0);
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}

pub fn sys_exec(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        task.exec(all_data.as_slice());
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    //trace!("kernel: sys_waitpid");
    let task = current_task().unwrap();
    // find a child process

    // ---- access current PCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB automatically
}

// Check the user bit as well as the requested access before touching RAM.
fn user_page(table: &PageTable, addr: usize, access: PTEFlags) -> Option<crate::mm::PhysPageNum> {
    let va = VirtAddr::from(addr);
    if usize::from(va) != addr {
        return None;
    }
    let entry = table.translate(va.floor())?;
    entry
        .flags()
        .contains(PTEFlags::V | PTEFlags::U | access)
        .then(|| entry.ppn())
}

// First validate all pages, so a bad later page never causes a partial write.
pub(super) fn copy_to_user(token: usize, start: usize, bytes: &[u8]) -> bool {
    let Some(end) = start.checked_add(bytes.len()) else {
        return false;
    };
    let table = PageTable::from_token(token);
    for addr in (start..end).step_by(crate::config::PAGE_SIZE) {
        if user_page(&table, addr, PTEFlags::W).is_none() {
            return false;
        }
    }
    if end > start && user_page(&table, end - 1, PTEFlags::W).is_none() {
        return false;
    }
    let mut addr = start;
    while addr < end {
        let ppn = user_page(&table, addr, PTEFlags::W).unwrap();
        let offset = VirtAddr::from(addr).page_offset();
        let chunk = (end - addr).min(crate::config::PAGE_SIZE - offset);
        ppn.get_bytes_array()[offset..offset + chunk]
            .copy_from_slice(&bytes[addr - start..addr - start + chunk]);
        addr += chunk;
    }
    true
}

/// Get time with second and microsecond, including across page boundaries.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    let us = get_time_us();
    let words = [us / 1_000_000, us % 1_000_000];
    // Two native-endian usize fields match the user ABI on RV64.
    let mut bytes = [0; core::mem::size_of::<TimeVal>()];
    let width = core::mem::size_of::<usize>();
    bytes[..width].copy_from_slice(&words[0].to_ne_bytes());
    bytes[width..].copy_from_slice(&words[1].to_ne_bytes());
    if copy_to_user(current_user_token(), ts as usize, &bytes) {
        0
    } else {
        -1
    }
}

/// Create an anonymous user mapping.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    if current_task()
        .unwrap()
        .inner_exclusive_access()
        .memory_set
        .mmap_area(start, len, prot)
    {
        0
    } else {
        -1
    }
}

/// Remove an anonymous user mapping.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    if current_task()
        .unwrap()
        .inner_exclusive_access()
        .memory_set
        .munmap_area(start, len)
    {
        0
    } else {
        -1
    }
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().pid.0);
    if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

/// Load a new child directly from an executable, without cloning its address space.
pub fn sys_spawn(path: *const u8) -> isize {
    let path = translated_str(current_user_token(), path);
    let Some(inode) = open_file(path.as_str(), OpenFlags::RDONLY) else {
        return -1;
    };
    let image = inode.read_all();
    if !image.starts_with(b"\x7fELF") {
        return -1;
    }
    let data = image.as_slice();
    let parent = current_task().unwrap();
    let child = Arc::new(crate::task::TaskControlBlock::new(data));
    let pid = child.getpid();
    {
        let mut inner = child.inner_exclusive_access();
        inner.parent = Some(Arc::downgrade(&parent));
        inner.fd_table = parent.inner_exclusive_access().fd_table.clone();
    }
    parent.inner_exclusive_access().children.push(child.clone());
    add_task(child);
    pid as isize
}

/// Set the running process's stride-scheduling priority.
pub fn sys_set_priority(prio: isize) -> isize {
    if prio < 2 {
        return -1;
    }
    current_task().unwrap().inner_exclusive_access().priority = prio as u64;
    prio
}
