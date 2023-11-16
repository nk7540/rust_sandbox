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
pub enum FileType {
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
                src_dir.insert("symlink".to_string(), FileType::Symlink("/home/cs_gakusei/work/rust_sandbox/src/noncredential".to_string()));
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
    static ref NEXT_FD: std::sync::RwLock<FileDescriptor> = std::sync::RwLock::new(3);  // start with 3
    static ref OPEN_FILES: std::sync::RwLock<HashMap<FileDescriptor, String>> = std::sync::RwLock::new(HashMap::new());
    static ref CURRENT_DIR: std::sync::RwLock<String> = std::sync::RwLock::new("/home/cs_gakusei/work/rust_sandbox".to_string());
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
    let open_files_lock = OPEN_FILES.read().unwrap();
    let current_dir = CURRENT_DIR.read().unwrap();

    let base_path = if path.starts_with("/") {
        "".to_string()
    } else if dirfd == libc::AT_FDCWD {
        current_dir.to_string()
    } else if let Some(dir_path) = open_files_lock.get(&dirfd) {
        dir_path.clone()
    } else {
        return -1;
    };
    let mut full_components: Vec<_> = base_path.split('/').filter(|&c| !c.is_empty()).collect();
    full_components.extend(components);

    if traverse_path(&fs_tree_lock, &full_components).is_some() {
        let mut fd = NEXT_FD.write().unwrap();
        let new_fd = *fd;
        *fd += 1;
        drop(fs_tree_lock);
        register_fd_in_proc(format!("/{}", full_components.join("/")).as_str(), new_fd);
        drop(open_files_lock);
        OPEN_FILES
            .write()
            .unwrap()
            .insert(new_fd, format!("/{}", full_components.join("/")));
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
    let components: Vec<&str> = path.split('/').filter(|&c| !c.is_empty()).collect();
    println!("readlinkat({}): FS_TREE.read()", path);
    let fs_tree_lock = FS_TREE.read().unwrap();
    let open_files_lock = OPEN_FILES.read().unwrap();

    // Determine the starting point in the filesystem based on dirfd
    let full_path: Vec<&str> = if dirfd == libc::AT_FDCWD {
        components
    } else {
        // We need to keep the lock guard in scope
        if let Some(dir_path) = open_files_lock.get(&dirfd) {
            // Combine the directory path with the provided pathname
            let mut base_path: Vec<&str> = dir_path.split('/').filter(|&c| !c.is_empty()).collect();
            base_path.extend(components);
            base_path
        } else {
            // Invalid dirfd
            return -1;
        }
    };

    // Resolve the symlink path within the filesystem tree starting from fs_tree
    if let Some(FileType::Symlink(target_path)) = traverse_path(&fs_tree_lock, &full_path) {
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

// pub unsafe fn remove(filename: *const c_char) -> c_int {
//     let path_str = CStr::from_ptr(filename).to_str().unwrap();
//     let path_components = parse_path(path_str);
//     let mut fs_tree_lock = FS_TREE.write().unwrap();

//     if path_components.is_empty() {
//         return -1; // Invalid path
//     }

//     let parent_path = if path_components.len() == 1 {
//         vec![] // If the path has only one component, then its parent is the root.
//     } else {
//         path_components[..path_components.len() - 1].to_vec()
//     };

//     let file_name = path_components.last().unwrap();

//     if let Some(FileType::Directory(ref mut parent_dir)) =
//         traverse_path_mut(&mut fs_tree_lock, &parent_path)
//     {
//         match parent_dir.get(*file_name) {
//             Some(FileType::Directory(contents)) if contents.is_empty() => {
//                 // Only allow removal of empty directories
//                 parent_dir.remove(*file_name);
//                 0 // Successfully removed
//             }
//             Some(FileType::Regular(_)) | Some(FileType::Symlink(_)) => {
//                 // Remove file or symlink
//                 parent_dir.remove(*file_name);
//                 0 // Successfully removed
//             }
//             _ => -1, // Directory not empty or file not found
//         }
//     } else {
//         -1 // Parent directory not found
//     }
// }

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
        if component == "." {
            continue;
        }
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

// fn traverse_path_mut<'a>(
//     current_dir: &'a mut HashMap<String, FileType>,
//     components: &[&str],
// ) -> Option<&'a mut FileType> {
//     let mut current = current_dir;
//     for &component in components {
//         match current.get_mut(component) {
//             Some(FileType::Directory(ref mut subdir)) => {
//                 current = subdir;
//             }
//             Some(file_type) if &component == components.last().unwrap() => {
//                 // Last component in path
//                 return Some(file_type);
//             }
//             _ => return None,
//         }
//     }
//     Some(&mut FileType::Directory(current.clone()))
// }

fn parse_path(path: &str) -> Vec<&str> {
    path.split('/').filter(|&c| !c.is_empty()).collect()
}
