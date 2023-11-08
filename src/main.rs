#[cfg(not(feature = "mock"))]
use libc::{open, openat, readlink};
#[cfg(feature = "mock")]
mod mockfs;
#[cfg(feature = "mock")]
use mockfs::{open, openat, readlink};
use std::ffi::CString;
use std::fs;

const MAX_PATH_SIZE: usize = 4096;
const DELIM: &str = "/";

fn process_component(component_path: &CString, fd: &mut i32) {
    let mut target_path = vec![0u8; MAX_PATH_SIZE];
    let target;

    let length = unsafe {
        readlink(
            component_path.as_ptr(),
            target_path.as_mut_ptr() as *mut i8,
            MAX_PATH_SIZE - 1,
        )
    };

    if length != -1 {
        target_path[length as usize] = 0;
        target = CString::new(target_path).unwrap();
    } else {
        target = component_path.clone();
    }

    let components = target.to_str().unwrap().split(DELIM);
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
}

fn safe_open(pathname: &str, mode: i32) -> i32 {
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
        return 1;
    }

    let components: Vec<_> = path.split(DELIM).collect();
    for component in components {
        process_component(&CString::new(component).unwrap(), &mut fd);
        // event: next_component
    }
    // event: fully_traversed
    // assert: property.txt
    fd
}

fn main() {
    let pathname = "./src/main.rs";
    let fd = safe_open(pathname, libc::O_RDONLY);
    println!("{}", fd);
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom::thread;

    #[test]
    #[should_panic]
    fn loom_test() {
        loom::model(|| {
            let t1 = thread::spawn(|| {
                let pathname = "./src/main.rs";
                let fd = safe_open(pathname, libc::O_RDONLY);
                let fd_path = format!("/proc/self/fd/{}", fd);
                let pointed_path = fs::read_link(&fd_path)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                assert!(pointed_path == pathname);
            });
            let t2 = thread::spawn(|| {
                open
            })
            t1.join()
        })
    }
}
