//! 极薄 syscall / IO 辅助层，只依赖 libc。
use std::ffi::CString;
use std::io::Write;

pub fn errno_text() -> String {
    let n = std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(0);
    let s = unsafe {
        let p = libc::strerror(n);
        if p.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(p)
                .to_string_lossy()
                .into_owned()
        }
    };
    format!("{} {}", n, s)
}

pub fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_else(|_| CString::new("").unwrap())
}

/// 直接 write(1)，绕开 std 缓冲（fork 后不会重复输出）
pub fn out(line: &str) {
    let s = format!("{}\n", line);
    unsafe {
        libc::write(1, s.as_ptr() as *const libc::c_void, s.len());
    }
}

pub const SEP: char = '\u{1}';

pub fn kv(status: &str, name: &str, detail: &str) {
    out(&format!("{}{}{}{}{}", status, SEP, name, SEP, detail));
}

/// 在子进程里跑 f()，输出用管道收回。子进程只 write(1) 后 _exit。
pub fn run_in_child<F: FnOnce()>(f: F) -> (bool, String) {
    let _ = std::io::stdout().flush();
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return (false, format!("pipe 失败 {}", errno_text()));
    }
    let (rfd, wfd) = (fds[0], fds[1]);
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return (false, format!("fork 失败 {}", errno_text()));
    }
    if pid == 0 {
        unsafe {
            libc::close(rfd);
            libc::dup2(wfd, 1);
            libc::dup2(wfd, 2);
            if wfd > 2 {
                libc::close(wfd);
            }
        }
        f();
        unsafe { libc::_exit(0) }
    }
    unsafe { libc::close(wfd) };
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = unsafe {
            libc::read(rfd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
        };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n as usize]);
    }
    unsafe { libc::close(rfd) };
    let mut st: libc::c_int = 0;
    unsafe { libc::waitpid(pid, &mut st, 0) };
    let exited = (st & 0x7f) == 0;
    let code = (st >> 8) & 0xff;
    (exited && code == 0, String::from_utf8_lossy(&buf).into_owned())
}

pub fn read_trim(p: &str) -> Option<String> {
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
}

pub fn grep_line(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find(|l| l.starts_with(key))
        .map(|l| l.to_string())
}
