#[cfg(not(feature = "mock"))]
use fs::read_link;
#[cfg(not(feature = "mock"))]
use libc::{open, openat, readlinkat};
mod mockfs;
#[cfg(feature = "mock")]
use mockfs::{open, openat, read_link, readlinkat};
use std::ffi::CString;
use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::Path;

const MAX_PATH_SIZE: usize = 4096;
const DELIM: &str = "/";
const DIRECTORY: &str = "/home/cs_gakusei/work/rust_sandbox/src/";
const CREDENTIALS: &str = "/home/cs_gakusei/work/rust_sandbox/src/credentials";
const NONCREDENTIAL: &str = "/home/cs_gakusei/work/rust_sandbox/src/noncredential";
const NONCREDENTIAL2: &str = "/home/cs_gakusei/work/rust_sandbox/src/noncredential2";
const SYMLINK: &str = "/home/cs_gakusei/work/rust_sandbox/src/symlink";

enum OpenError {
    AccessDenied,
    OpenError,
}

fn process_component(component_path: &CString, fd: &mut i32) -> bool {
    let mut target_path = vec![0u8; MAX_PATH_SIZE];

    let length = unsafe {
        readlinkat(
            *fd,
            component_path.as_ptr(),
            target_path.as_mut_ptr() as *mut i8,
            MAX_PATH_SIZE - 1,
        )
    };

    let target = if length != -1 {
        unsafe { target_path.set_len(length as usize) };
        CString::new(target_path).unwrap()
    } else {
        component_path.clone()
    };
    let mut target = target.to_str().unwrap();

    // policy checking
    let full_path = if target.starts_with(DELIM) {
        target.to_owned()
    } else {
        let proc_path = format!("/proc/self/fd/{}", fd);
        let full_path = read_link(proc_path).unwrap().join(target);
        full_path.to_str().unwrap().to_owned()
    };
    if full_path == CREDENTIALS {
        return false;
    }

    // if the content of the symlink is absolute, reset the fd and traverse
    if target.starts_with(DELIM) {
        *fd = unsafe { open(CString::new(DELIM).unwrap().as_ptr(), libc::O_RDONLY) };
        target = &target[1..];
    }
    let components = target.split(DELIM);
    for target_component in components {
        *fd = unsafe {
            openat(
                *fd,
                CString::new(target_component).unwrap().as_ptr(),
                libc::O_NOFOLLOW,
            )
        };
        // event: open_nonsym
    }
    true
}

fn safe_open(pathname: &str, mode: i32) -> Result<i32, OpenError> {
    let mut fd;
    let mut path = pathname;

    if path.starts_with(DELIM) {
        // event: absolute
        fd = unsafe { open(CString::new(DELIM).unwrap().as_ptr(), mode) };
        // event: root_opened
        path = &path[1..];
        // event: made_relative
    } else {
        // event: not_absolute
        fd = unsafe { open(CString::new(".").unwrap().as_ptr(), mode) };
        // event: cwd_opened
    }

    if fd == -1 {
        eprintln!("Error opening directory");
        return Err(OpenError::OpenError);
    }

    let components: Vec<_> = path.split(DELIM).collect();
    for component in components {
        let res = process_component(&CString::new(component).unwrap(), &mut fd);
        if !res {
            return Err(OpenError::AccessDenied);
        }
        // event: next_component
    }
    // event: fully_traversed
    // assert: property.txt
    if fd == -1 {
        Err(OpenError::OpenError)
    } else {
        Ok(fd)
    }
}

fn main() {
    let pathname = "./src/noncredential";
    let res = safe_open(pathname, libc::O_RDONLY);
    match res {
        Ok(fd) => println!("{}", fd),
        Err(OpenError::AccessDenied) => println!("denied"),
        Err(OpenError::OpenError) => println!("error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom::thread;
    use std::os::fd::AsRawFd;

    #[test]
    fn test_safe_open() {
        // so that the noncredential file is not a symlink at first
        fs::remove_file(NONCREDENTIAL);
        fs::File::create(NONCREDENTIAL);
        create_symlink(NONCREDENTIAL, SYMLINK);

        loom::model(|| {
            // replace the link so it points to another file denied to access
            let t2 = thread::spawn(|| {
                create_symlink(CREDENTIALS, NONCREDENTIAL);
            });
            let t3 = thread::spawn(|| {
                create_symlink(NONCREDENTIAL2, NONCREDENTIAL);
            });
            // make sure that it does not allow the access to the newly-pointed file
            let res = safe_open(SYMLINK, libc::O_RDONLY);
            if res.is_ok() {
                let fd_path = format!("/proc/self/fd/{}", res.unwrap_or_default());
                println!("safe_open: {}", fd_path);
                let pointed_path = read_link(&fd_path).unwrap().to_string_lossy().into_owned();
                assert_eq!(pointed_path, NONCREDENTIAL);
            } else {
                // println!("{}", res.unwrap_err());
                println!("open error");
            }
        })
    }

    #[test]
    fn test_unsafe_open() {
        // so that the noncredential file is not a symlink at first
        fs::remove_file(NONCREDENTIAL);
        fs::File::create(NONCREDENTIAL);
        create_symlink(NONCREDENTIAL, SYMLINK);

        loom::model(|| {
            let t2 = thread::spawn(|| {
                println!("unsafe_open: t2");
                create_symlink(CREDENTIALS, NONCREDENTIAL);
                // create_symlink(NONCREDENTIAL2, NONCREDENTIAL);
            });
            let t3 = thread::spawn(|| {
                println!("unsafe_open: t3");
                // create_symlink(CREDENTIALS, NONCREDENTIAL);
                create_symlink(NONCREDENTIAL2, NONCREDENTIAL);
            });
            t2.join();
            t3.join();
            let target = read_link(SYMLINK).unwrap();
            let target = target.to_str().unwrap();
            if target != CREDENTIALS {
                let file = fs::File::open(target).unwrap(); // assume it's absolute
                let fd_path = format!("/proc/self/fd/{}", file.as_raw_fd());
                println!("unsafe_open: {}", fd_path);
                let pointed_path = read_link(&fd_path).unwrap().to_string_lossy().into_owned();
                assert_ne!(pointed_path, CREDENTIALS);
            }
        })
    }

    fn create_symlink(original_path: &str, link_path: &str) -> std::io::Result<()> {
        let original = Path::new(original_path);
        let link = Path::new(link_path);
        if link.exists() || link.symlink_metadata().is_ok() {
            std::fs::remove_file(link)?;
        }

        unix_fs::symlink(original, link)
    }
}
