#[cfg(not(loom))]
use lazy_static::lazy_static;
use libc::{O_RDONLY, O_RDWR, O_WRONLY};
#[cfg(loom)]
use loom::lazy_static;
#[cfg(loom)]
use loom::sync::RwLock;
use std::collections::HashMap;
use std::ffi::CStr;
use std::ffi::CString;
use std::io;
use std::os::raw::{c_char, c_int};
use std::path::Path;
#[cfg(not(loom))]
use std::sync::RwLock;

type FileDescriptor = c_int;

#[derive(Debug, Clone)]
enum FileType {
    Regular(String),                      // Contains file content
    Directory(HashMap<String, FileType>), // A map of file names to file types
    Symlink(String),                      // Contains the target path
}

lazy_static! {
    static ref FS_TREE: RwLock<HashMap<String, FileType>> = {
        let mut m = HashMap::new();
        // Create directories and files based on the provided structure.
        let mut rust_sandbox_dir = HashMap::new();
        rust_sandbox_dir.insert(
            "src".to_string(),
            FileType::Directory({
                let mut src_dir = HashMap::new();
                src_dir.insert("credentials".to_string(), FileType::Regular("credentials content".to_string()));
                src_dir.insert("noncredential".to_string(), FileType::Regular("noncredential content".to_string()));
                src_dir.insert("noncredential2".to_string(), FileType::Regular("noncredential2 content".to_string()));
                src_dir.insert("symlink".to_string(), FileType::Symlink("/home/cs_gakusei/work/rust_sandbox/src/credentials".to_string()));
                src_dir
            }),
        );

        m.insert(
            "home".to_string(),
            FileType::Directory({
                let mut home_dir = HashMap::new();
                home_dir.insert("cs_gakusei".to_string(), FileType::Directory({
                    let mut cs_gakusei_dir = HashMap::new();
                    cs_gakusei_dir.insert("work".to_string(), FileType::Directory({
                        let mut work_dir = HashMap::new();
                        work_dir.insert("rust_sandbox".to_string(), FileType::Directory(rust_sandbox_dir));
                        work_dir
                    }));
                    cs_gakusei_dir
                }));
                home_dir
            }),
        );
        RwLock::new(m)
    };
    static ref NEXT_FD: RwLock<FileDescriptor> = RwLock::new(3);  // start with 3
    static ref OPEN_FILES: RwLock<HashMap<FileDescriptor, String>> = RwLock::new(HashMap::new());
    static ref CURRENT_DIR: RwLock<String> = RwLock::new("/home/cs_gakusei/work/rust_sandbox".to_string());
}

pub unsafe fn open(path: *const c_char, oflag: c_int) -> c_int {
    openat(libc::AT_FDCWD, path, oflag)
}

pub unsafe fn openat(dirfd: c_int, pathname: *const c_char, flags: c_int) -> c_int {
    let path = CStr::from_ptr(pathname).to_str().unwrap_or("");
    let path = convert_relative_to_absolute_path(path);
    let components: Vec<&str> = path.split('/').filter(|&c| !c.is_empty()).collect();

    let fs_tree_lock = FS_TREE.read().unwrap();
    let fs_tree = if dirfd == libc::AT_FDCWD {
        &*fs_tree_lock // If dirfd is AT_FDCWD, we use the root of the FS tree.
    } else if let Some(dir_path) = OPEN_FILES.read().unwrap().get(&dirfd) {
        // Here we assume that dirfd is a directory.
        // In a real file system, you would check that dirfd is indeed a directory.
        if let Some(FileType::Directory(contents)) = fs_tree_lock.get(dir_path) {
            contents
        } else {
            return -1;
        }
    } else {
        return -1; // Invalid dirfd
    };

    if let Some(file_type) = traverse_path(&fs_tree, &components) {
        let mut fd = NEXT_FD.write().unwrap();
        let new_fd = *fd;
        *fd += 1;
        drop(fs_tree); // Drop the read lock before calling register_fd_in_proc
        register_fd_in_proc(path.as_str(), new_fd);
        OPEN_FILES.write().unwrap().insert(new_fd, path.to_string());
        new_fd
    } else {
        -1
    }
}

pub unsafe fn readlinkat(
    dirfd: c_int,
    pathname: *const c_char,
    buf: *mut c_char,
    bufsz: usize,
) -> isize {
    let path = CStr::from_ptr(pathname).to_str().unwrap_or("");
    let fs_tree_lock = FS_TREE.read().unwrap();

    // Resolve the starting point in the filesystem based on dirfd
    let fs_tree = if dirfd == libc::AT_FDCWD {
        &*fs_tree_lock // Use the root of the filesystem tree if dirfd is AT_FDCWD
    } else if let Some(dir_path) = OPEN_FILES.read().unwrap().get(&dirfd) {
        // If dirfd is a valid directory file descriptor, find the directory in the filesystem tree
        if let Some(FileType::Directory(contents)) = fs_tree_lock.get(dir_path) {
            contents
        } else {
            return -1; // dirfd is not a directory
        }
    } else {
        return -1; // Invalid dirfd
    };

    // Resolve the symlink path within the filesystem tree starting from fs_tree
    if let Some(FileType::Symlink(target_path)) = fs_tree.get(path) {
        let bytes_to_copy = target_path.as_bytes().len().min(bufsz);
        for (i, byte) in target_path.as_bytes()[..bytes_to_copy].iter().enumerate() {
            *buf.add(i) = *byte as c_char;
        }
        bytes_to_copy as isize
    } else {
        -1 // Path does not exist or is not a symlink
    }
}

pub fn read_link<P: AsRef<Path>>(path: P) -> io::Result<std::path::PathBuf> {
    let c_path = CString::new(path.as_ref().to_str().unwrap()).unwrap();
    // Allocate a buffer for the link path. Adjust the size as necessary.
    let mut buf = vec![0; 1024];
    let res = unsafe {
        readlinkat(
            libc::AT_FDCWD,
            c_path.as_ptr(),
            buf.as_mut_ptr() as *mut c_char,
            buf.len(),
        )
    };

    if res >= 0 {
        let end = res as usize;
        buf.truncate(end);
        // Convert the C string to a Rust string.
        let c_str = unsafe { CString::from_vec_unchecked(buf) };
        let str_slice = c_str
            .to_str()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid UTF-8"))?;
        Ok(Path::new(str_slice).to_path_buf())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn create(path: &str, file_type: FileType) -> Result<(), &'static str> {
    let mut components = path.split('/').collect::<Vec<_>>();
    if components.is_empty() {
        return Err("Invalid path");
    }

    // Handle absolute paths
    if path.starts_with('/') {
        components.remove(0);
    }

    let mut fs_tree = FS_TREE.write().unwrap();

    let mut current_path = String::new();
    for component in components.iter().take(components.len() - 1) {
        current_path.push('/');
        current_path.push_str(component);

        let entry = fs_tree
            .entry(current_path.clone())
            .or_insert_with(|| FileType::Directory(HashMap::new()));
        if let FileType::Directory(ref mut subdir) = entry {
            // This now correctly references the subdir HashMap.
            // No need to create a new RwLock.
            fs_tree = subdir;
        } else {
            // The path component exists and is not a directory
            return Err("Path component is not a directory");
        }
    }

    // Now `fs_tree` is the parent directory of the file we want to create
    let name = components.last().unwrap();
    current_path.push('/');
    current_path.push_str(name);

    if fs_tree.contains_key(&current_path) {
        // The file already exists
        Err("File already exists")
    } else {
        fs_tree.insert(current_path, file_type);
        Ok(())
    }
}

fn register_fd_in_proc(path: &str, fd: c_int) {
    let proc_entry = format!("/proc/self/fd/{}", fd);
    let mut fs_tree = FS_TREE.write().unwrap();

    // Inserting the fd as a symlink to the actual path in our mock /proc/self/fd
    fs_tree.insert(proc_entry, FileType::Symlink(path.to_string()));
}

fn convert_relative_to_absolute_path(relative_path: &str) -> String {
    let current_dir = CURRENT_DIR.read().unwrap();
    if relative_path.starts_with("/") {
        // Already an absolute path, return as is.
        relative_path.to_string()
    } else if relative_path == "." {
        // The current directory is requested.
        current_dir.to_string()
    } else {
        // A relative path is given, join it with the current directory.
        let mut path = current_dir.clone();
        if !path.ends_with("/") {
            path.push('/');
        }
        path.push_str(relative_path);
        path
    }
}

fn traverse_path<'a>(
    current_dir: &'a HashMap<String, FileType>,
    components: &[&str],
) -> Option<FileType> {
    let mut current = current_dir;
    for &component in components.iter() {
        match current.get(component) {
            Some(FileType::Directory(ref subdir)) => {
                current = subdir;
            }
            Some(file_type) if &component == components.last().unwrap() => {
                // Last component in path
                return Some(file_type.clone());
            }
            _ => return None,
        }
    }
    Some(FileType::Directory(current.clone()))
}
