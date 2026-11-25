//! Process management syscalls
use crate::mm::{PTEFlags, PageTable, VirtAddr};
use crate::task::{
    change_program_brk, current_user_token, exit_current_and_run_next, mmap_current,
    munmap_current, suspend_current_and_run_next,
};
use crate::timer::get_time_us;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

// Lab 2 reference (permission checks and cross-page time values):
// https://learningos.cn/rCore-Tutorial-Guide/chapter4/7exercise.html
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

/// Trace user memory and per-task syscall counts.
pub fn sys_trace(request: usize, id: usize, data: usize) -> isize {
    if request == 2 {
        return crate::task::syscall_count(id) as isize;
    }
    let table = PageTable::from_token(current_user_token());
    let offset = VirtAddr::from(id).page_offset();
    match request {
        0 => user_page(&table, id, PTEFlags::R)
            .map_or(-1, |ppn| ppn.get_bytes_array()[offset] as isize),
        1 => {
            if let Some(ppn) = user_page(&table, id, PTEFlags::W) {
                ppn.get_bytes_array()[offset] = data as u8;
                0
            } else {
                -1
            }
        }
        _ => -1,
    }
}

/// Create an anonymous user mapping.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    if mmap_current(start, len, prot) {
        0
    } else {
        -1
    }
}

/// Remove an anonymous user mapping.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    if munmap_current(start, len) {
        0
    } else {
        -1
    }
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
