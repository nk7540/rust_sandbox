#[cfg(not(loom))]
use lazy_static::lazy_static;
use libc::{O_RDONLY, O_RDWR, O_WRONLY};
#[cfg(loom)]
use loom::lazy_static;
use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::sync::RwLock;

type FileDescriptor = c_int;

#[derive(Debug, Clone)]
enum FileType {
    Regular(String), // Contains file content
    Directory,
    Symlink(String), // Contains the target path
}

lazy_static! {
    static ref FS_TREE: RwLock<HashMap<String, FileType>> = RwLock::new(HashMap::new());
    static ref NEXT_FD: RwLock<FileDescriptor> = RwLock::new(3);  // start with 3
    static ref OPEN_FILES: RwLock<HashMap<FileDescriptor, String>> = RwLock::new(HashMap::new());
}

pub unsafe fn open(path: *const c_char, oflag: c_int) -> c_int {
    openat(-1, path, oflag)
}

pub unsafe fn openat(_dirfd: c_int, pathname: *const c_char, flags: c_int) -> c_int {
    let path = CStr::from_ptr(pathname).to_str().unwrap_or("");

    let fs_tree = FS_TREE.read().unwrap();

    match fs_tree.get(path) {
        Some(FileType::Regular(_)) => {
            if (flags & O_RDONLY) != 0 || (flags & O_WRONLY) != 0 || (flags & O_RDWR) != 0 {
                let mut fd = NEXT_FD.write().unwrap();
                let new_fd = *fd;
                *fd += 1;
                OPEN_FILES.write().unwrap().insert(new_fd, path.to_string());
                new_fd
            } else {
                -1 // Other flags are not implemented for simplicity
            }
        }
        _ => -1, // file does not exist or is not a regular file
    }
}

pub unsafe fn readlink(path: *const c_char, buf: *mut c_char, bufsz: usize) -> isize {
    let path = CStr::from_ptr(path).to_str().unwrap_or("");
    let fs_tree = FS_TREE.read().unwrap();

    if let Some(FileType::Symlink(target_path)) = fs_tree.get(path) {
        let bytes_to_copy = target_path.as_bytes().len().min(bufsz);
        for (i, byte) in target_path.as_bytes()[..bytes_to_copy].iter().enumerate() {
            *buf.add(i) = *byte as c_char;
        }
        bytes_to_copy as isize
    } else {
        -1 // Not a symlink or does not exist
    }
}
