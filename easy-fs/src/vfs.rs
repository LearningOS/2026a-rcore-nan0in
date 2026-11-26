use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ,
};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};
/// Virtual filesystem layer over easy-fs
pub struct Inode {
    block_id: usize,
    block_offset: usize,
    fs: Arc<Mutex<EasyFileSystem>>,
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a vfs inode
    pub fn new(
        block_id: u32,
        block_offset: usize,
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
    ) -> Self {
        Self {
            block_id: block_id as usize,
            block_offset,
            fs,
            block_device,
        }
    }
    /// Call a function over a disk inode to read it
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// Call a function over a disk inode to modify it
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// Find inode under a disk inode by name
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory
        assert!(disk_inode.is_dir());
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        for i in 0..file_count {
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                DIRENT_SZ,
            );
            if dirent.inode_id() != 0 && dirent.name() == name {
                return Some(dirent.inode_id() as u32);
            }
        }
        None
    }
    /// Find inode under current inode by name
    pub fn find(&self, name: &str) -> Option<Arc<Inode>> {
        let fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            self.find_inode_id(name, disk_inode).map(|inode_id| {
                let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
                Arc::new(Self::new(
                    block_id,
                    block_offset,
                    self.fs.clone(),
                    self.block_device.clone(),
                ))
            })
        })
    }
    /// Increase the size of a disk inode
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size < disk_inode.size {
            return;
        }
        let blocks_needed = disk_inode.blocks_num_needed(new_size);
        let mut v: Vec<u32> = Vec::new();
        for _ in 0..blocks_needed {
            v.push(fs.alloc_data());
        }
        disk_inode.increase_size(new_size, v, &self.block_device);
    }
    /// Create inode under current inode by name
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        if !Self::valid_name(name) {
            return None;
        }
        let mut fs = self.fs.lock();
        let op = |root_inode: &DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(name, root_inode)
        };
        if self.read_disk_inode(op).is_some() {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();
        // initialize inode
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {
                new_inode.initialize(DiskInodeType::File);
            });
        self.insert_entry(name, new_inode_id, &mut fs);

        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);
        block_cache_sync_all();
        // return inode
        Some(Arc::new(Self::new(
            block_id,
            block_offset,
            self.fs.clone(),
            self.block_device.clone(),
        )))
        // release efs lock automatically by compiler
    }
    fn valid_name(name: &str) -> bool {
        !name.is_empty() && name.len() <= DIRENT_SZ - 5 && !name.contains('/')
    }

    fn insert_entry(&self, name: &str, id: u32, fs: &mut MutexGuard<EasyFileSystem>) {
        self.modify_disk_inode(|dir| {
            let count = dir.size as usize / DIRENT_SZ;
            let mut entry = DirEntry::empty();
            let index = (0..count)
                .find(|i| {
                    dir.read_at(i * DIRENT_SZ, entry.as_bytes_mut(), &self.block_device);
                    entry.inode_id() == 0
                })
                .unwrap_or(count);
            if index == count {
                self.increase_size(((count + 1) * DIRENT_SZ) as u32, dir, fs);
            }
            dir.write_at(
                index * DIRENT_SZ,
                DirEntry::new(name, id).as_bytes(),
                &self.block_device,
            );
        });
    }

    /// The stable index of this inode in the filesystem's inode table.
    pub fn id(&self) -> u32 {
        let fs = self.fs.lock();
        let (first, _) = fs.get_disk_inode_pos(0);
        let size = core::mem::size_of::<DiskInode>();
        ((self.block_id - first as usize) * (crate::BLOCK_SZ / size) + self.block_offset / size)
            as u32
    }

    /// Count names referring to the inode in this flat directory.
    pub fn link_count(&self, id: u32) -> u32 {
        let _fs = self.fs.lock();
        self.read_disk_inode(|dir| {
            let mut entry = DirEntry::empty();
            (0..dir.size as usize / DIRENT_SZ)
                .filter(|index| {
                    dir.read_at(index * DIRENT_SZ, entry.as_bytes_mut(), &self.block_device);
                    entry.inode_id() != 0 && entry.inode_id() == id
                })
                .count() as u32
        })
    }

    /// Add a second name for an existing file.
    pub fn link(&self, old: &str, new: &str) -> bool {
        if !Self::valid_name(new) {
            return false;
        }
        let mut fs = self.fs.lock();
        let id = self.read_disk_inode(|dir| {
            if self.find_inode_id(new, dir).is_some() {
                return None;
            }
            self.find_inode_id(old, dir)
        });
        let Some(id) = id else {
            return false;
        };
        self.insert_entry(new, id, &mut fs);
        block_cache_sync_all();
        true
    }

    /// Remove a name without invalidating existing open handles.
    pub fn unlink(&self, name: &str) -> Option<u32> {
        let _fs = self.fs.lock();
        let id = self.modify_disk_inode(|dir| {
            let mut entry = DirEntry::empty();
            for i in 0..dir.size as usize / DIRENT_SZ {
                dir.read_at(i * DIRENT_SZ, entry.as_bytes_mut(), &self.block_device);
                if entry.inode_id() != 0 && entry.name() == name {
                    dir.write_at(
                        i * DIRENT_SZ,
                        DirEntry::empty().as_bytes(),
                        &self.block_device,
                    );
                    return Some(entry.inode_id());
                }
            }
            None
        });
        block_cache_sync_all();
        id
    }

    /// Reclaim an inode after its last name and last open handle have gone.
    pub fn reclaim_unlinked(&self, id: u32) {
        let (block, offset) = self.fs.lock().get_disk_inode_pos(id);
        let target = Self::new(block, offset, self.fs.clone(), self.block_device.clone());
        target.clear();
        self.fs.lock().dealloc_inode(id);
        block_cache_sync_all();
    }

    /// List inodes under current inode
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut v: Vec<String> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                if dirent.inode_id() != 0 {
                    v.push(String::from(dirent.name()));
                }
            }
            v
        })
    }
    /// Read data from current inode
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write data to current inode
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        block_cache_sync_all();
        size
    }
    /// Clear the data in current inode
    pub fn clear(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }
}
