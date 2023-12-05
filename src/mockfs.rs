#[cfg(not(loom))]
use lazy_static::lazy_static as lazy_static_loom;
use libc::{O_RDONLY, O_RDWR, O_WRONLY};
#[cfg(loom)]
use loom::lazy_static as lazy_static_loom;
#[cfg(loom)]
use loom::sync::RwLock;
#[cfg(loom)]
use loom::thread_local;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CStr;
use std::ffi::CString;
use std::io;
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::sync::Arc;
#[cfg(not(loom))]
use std::sync::RwLock;

type FileDescriptor = c_int;

#[derive(Debug, Clone)]
pub enum FileType {
    Regular(String),                      // Contains file content
    Directory(HashMap<String, FileType>), // A map of file names to file types
    Symlink(String),                      // Contains the target path
}

lazy_static_loom! {
    static ref FS_TREE: RwLock<HashMap<String, FileType>> = RwLock::new(HashMap::new());
}

thread_local! {
    static NEXT_FD: RefCell<FileDescriptor> = RefCell::new(3);
    static OPEN_FILES: RefCell<HashMap<FileDescriptor, String>> = RefCell::new(HashMap::new());
    static CURRENT_DIR: RefCell<String> = RefCell::new("/home/cs_gakusei/work/rust_sandbox".to_string());
}

pub fn initialize_mockfs() {
    create(
        "/home/cs_gakusei/work/rust_sandbox/src/noncredential",
        FileType::Regular("noncredential content".to_string()),
    )
    .unwrap();
    create(
        "/home/cs_gakusei/work/rust_sandbox/src/credentials",
        FileType::Regular("credentials content".to_string()),
    )
    .unwrap();
    create(
        "/home/cs_gakusei/work/rust_sandbox/src/symlink",
        FileType::Symlink("/home/cs_gakusei/work/rust_sandbox/src/noncredential".to_string()),
    )
    .unwrap();
}

pub unsafe fn open(path: *const c_char, oflag: c_int) -> c_int {
    openat(libc::AT_FDCWD, path, oflag)
}

pub unsafe fn openat(dirfd: c_int, pathname: *const c_char, flags: c_int) -> c_int {
    let path = CStr::from_ptr(pathname).to_str().unwrap_or("");
    let components: Vec<&str> = path
        .split('/')
        .filter(|&c| !c.is_empty() && c != ".")
        .collect();
    println!("openat({}): FS_TREE.read()", path);
    let fs_tree_lock = FS_TREE.read().unwrap();

    let base_path = if path.starts_with("/") {
        "".to_string()
    } else if dirfd == libc::AT_FDCWD {
        CURRENT_DIR.with(|v| v.borrow().clone()).to_string()
    } else if let Some(dir_path) = OPEN_FILES.with(|v| v.borrow().clone()).get(&dirfd) {
        dir_path.clone()
    } else {
        return -1;
    };
    let mut full_components: Vec<_> = base_path.split('/').filter(|&c| !c.is_empty()).collect();
    full_components.extend(components);

    if let Some((_, resolved_path)) =
        traverse_path_recursive(&fs_tree_lock, &full_components, flags)
    {
        drop(fs_tree_lock);
        NEXT_FD.with(|next_fd| {
            let new_fd = *next_fd.borrow();
            register_fd_in_proc(resolved_path.as_str(), new_fd);
            OPEN_FILES.with(|open_files| (*open_files.borrow_mut()).insert(new_fd, resolved_path));
            *next_fd.borrow_mut() += 1;
            new_fd
        })
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
    let components: Vec<&str> = path.split('/').filter(|&c| !c.is_empty()).collect();
    println!("readlinkat({}): FS_TREE.read()", path);
    let fs_tree_lock = FS_TREE.read().unwrap();

    // Determine the starting point in the filesystem based on dirfd
    let base_path = if path.starts_with("/") {
        "".to_string()
    } else if dirfd == libc::AT_FDCWD {
        CURRENT_DIR.with(|v| v.borrow().clone()).to_string()
    } else if let Some(dir_path) = OPEN_FILES.with(|v| v.borrow().clone()).get(&dirfd) {
        dir_path.clone()
    } else {
        return -1;
    };
    let mut full_components: Vec<_> = base_path.split('/').filter(|&c| !c.is_empty()).collect();
    full_components.extend(components);

    // Resolve the symlink path within the filesystem tree starting from fs_tree
    if let Some((file_type, _)) = traverse_path(&fs_tree_lock, &full_components) {
        drop(fs_tree_lock);
        let target_path = if let FileType::Symlink(dst_path) = file_type {
            dst_path
        } else {
            path.to_string() // EINVAL in readlink(2) but returns the original path for simplicity
        };
        let bytes_to_copy = target_path.as_bytes().len().min(bufsz);
        for (i, byte) in target_path.as_bytes()[..bytes_to_copy].iter().enumerate() {
            *buf.add(i) = *byte as c_char;
        }
        bytes_to_copy as isize
    } else {
        drop(fs_tree_lock);
        -1 // Path does not exist
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

    println!("create({}): FS_TREE.write()", path);
    let mut fs_tree_guard = FS_TREE.write().unwrap();

    // Initialize `sub_tree` as a mutable reference to `FS_TREE`.
    let mut sub_tree = &mut *fs_tree_guard;

    for component in components.iter().take(components.len() - 1) {
        // This will create a new directory if it doesn't exist
        sub_tree = sub_tree
            .entry(component.to_string())
            .or_insert_with(|| FileType::Directory(HashMap::new()))
            .as_directory_mut()?; // Convert to &mut HashMap or return an error if not a directory
    }

    // Insert the file or symlink at the appropriate place in the tree
    let name = components.last().unwrap();
    if sub_tree.contains_key(*name) {
        Err("File already exists")
    } else {
        sub_tree.insert(name.to_string(), file_type);
        Ok(())
    }
}

trait AsDirectoryMut {
    fn as_directory_mut(&mut self) -> Result<&mut HashMap<String, FileType>, &'static str>;
}

impl AsDirectoryMut for FileType {
    fn as_directory_mut(&mut self) -> Result<&mut HashMap<String, FileType>, &'static str> {
        match self {
            FileType::Directory(ref mut map) => Ok(map),
            _ => Err("Not a directory"),
        }
    }
}

pub unsafe fn remove(filename: *const c_char) -> c_int {
    let path_str = CStr::from_ptr(filename).to_str().unwrap();
    let path_components = parse_path(path_str);
    println!("remove({}): FS_TREE.write()", path_str);
    let mut fs_tree_lock = FS_TREE.write().unwrap();

    if path_components.is_empty() {
        return -1; // Invalid path
    }

    let parent_path = if path_components.len() == 1 {
        vec![] // If the path has only one component, then its parent is the root.
    } else {
        path_components[..path_components.len() - 1].to_vec()
    };

    let file_name = path_components.last().unwrap();

    if let Some(FileType::Directory(ref mut parent_dir)) =
        traverse_path_mut(&mut fs_tree_lock, &parent_path)
    {
        match parent_dir.get(*file_name) {
            Some(FileType::Directory(contents)) if contents.is_empty() => {
                // Only allow removal of empty directories
                parent_dir.remove(*file_name);
                0 // Successfully removed
            }
            Some(FileType::Regular(_)) | Some(FileType::Symlink(_)) => {
                // Remove file or symlink
                parent_dir.remove(*file_name);
                0 // Successfully removed
            }
            _ => -1, // Directory not empty or file not found
        }
    } else {
        -1 // Parent directory not found
    }
}

pub unsafe fn link(src: *const c_char, dst: *const c_char) -> c_int {
    let src_str = CStr::from_ptr(src).to_str().unwrap();
    let dst_str = CStr::from_ptr(dst).to_str().unwrap();

    println!("link({}, {}): FS_TREE.read()", src_str, dst_str);
    let fs_tree_lock = FS_TREE.read().unwrap();
    if let Some(_) = traverse_path(&fs_tree_lock, &parse_path(src_str)) {
        drop(fs_tree_lock);
        match create(dst_str, FileType::Symlink(src_str.to_string())) {
            Ok(_) => 0,   // Success
            Err(_) => -1, // Failed to create symlink
        }
    } else {
        -1 // Source path does not exist
    }
}

// pub unsafe fn unlink(path: *const c_char) -> c_int {
//     let path_str = CStr::from_ptr(path).to_str().unwrap();
//     let path_components = parse_path(path_str);
//     let mut fs_tree_lock = FS_TREE.write().unwrap();

//     let parent_path = path_components[..path_components.len() - 1].to_vec();
//     let file_name = path_components.last().unwrap();

//     if let Some(FileType::Directory(ref mut parent_dir)) =
//         traverse_path_mut(&mut fs_tree_lock, &parent_path)
//     {
//         if parent_dir.remove(*file_name).is_some() {
//             0 // Successfully removed
//         } else {
//             -1 // File not found
//         }
//     } else {
//         -1 // Parent directory not found
//     }
// }

fn register_fd_in_proc(path: &str, fd: c_int) {
    let proc_entry = format!("/proc/self/fd/{}", fd);
    create(&proc_entry, FileType::Symlink(path.to_string()));
}

fn convert_relative_to_absolute_path(relative_path: &str) -> String {
    if relative_path.starts_with("/") {
        // Already an absolute path, return as is.
        relative_path.to_string()
    } else if relative_path == "." {
        // The current directory is requested.
        CURRENT_DIR.with(|v| v.borrow().clone()).to_string()
    } else {
        // A relative path is given, join it with the current directory.
        let mut path = CURRENT_DIR.with(|v| v.borrow().clone()).clone();
        if !path.ends_with("/") {
            path.push('/');
        }
        path.push_str(relative_path);
        path
    }
}

fn traverse_path<'a>(
    root: &'a HashMap<String, FileType>,
    components: &[&str],
) -> Option<(FileType, String)> {
    let mut current = root;
    let mut path = Vec::from(components);
    let mut resolved_path = String::new();

    while let Some(component) = path.first() {
        resolved_path.push_str("/");
        resolved_path.push_str(component);

        if *component == "." {
            path.remove(0);
            continue;
        }

        match current.get(*component) {
            Some(FileType::Directory(ref subdir)) if path.len() > 1 => {
                current = subdir;
                path.remove(0);
            }
            Some(FileType::Symlink(target)) if path.len() > 1 => {
                let mut target_components = target
                    .split('/')
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>();
                path.remove(0);
                target_components.append(&mut path);
                path = target_components;
                current = root; // Restart from root because symlink target is an absolute path
                resolved_path = String::new();
            }
            Some(file_type) if path.len() == 1 => {
                return Some((file_type.clone(), resolved_path));
            }
            _ => return None,
        }
    }

    Some((FileType::Directory(current.clone()), resolved_path))
}

fn traverse_path_recursive<'a>(
    root: &'a HashMap<String, FileType>,
    components: &[&str],
    flags: i32,
) -> Option<(FileType, String)> {
    let (file_type, path) = traverse_path(root, components)?;

    match file_type {
        FileType::Symlink(target_path) => {
            if flags == libc::O_NOFOLLOW {
                return None;
            }
            let target_components = target_path
                .split('/')
                .filter(|c| !c.is_empty())
                .collect::<Vec<_>>();
            traverse_path_recursive(root, &target_components, flags)
        }
        _ => Some((file_type, path)),
    }
}

fn traverse_path_mut<'a>(
    current_dir: &'a mut HashMap<String, FileType>,
    components: &[&str],
) -> Option<&'a mut FileType> {
    let mut current = current_dir;
    let path_len = components.len();

    for (index, &component) in components.iter().enumerate() {
        if let Some(next) = current.get_mut(component) {
            if index == path_len - 1 {
                return Some(next);
            }

            if let FileType::Directory(ref mut subdir) = next {
                current = subdir;
            } else {
                // A non-directory component was found where a directory was expected
                return None;
            }
        } else {
            // Component not found
            return None;
        }
    }

    None
}

fn parse_path(path: &str) -> Vec<&str> {
    path.split('/').filter(|&c| !c.is_empty()).collect()
}
