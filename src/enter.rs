//! enter：进入容器（setns + fork + exec）。
//! setns(CLONE_NEWPID) 只对之后 fork 的子进程生效，所以必须 fork。
use crate::config::Container;
use crate::util;
use std::path::Path;

pub fn enter(c: &Container, argv: &[String]) -> i32 {
    if !c.running() {
        eprintln!("容器 {} 没在运行（先 vibego start {}）",
                  c.name, c.name);
        return 1;
    }
    let pid = c.read_pid().unwrap_or(0);
    let names = ["mnt", "uts", "ipc", "cgroup", "pid"];
    let mut fds: Vec<libc::c_int> = Vec::new();
    for n in names {
        let p = format!("/proc/{}/ns/{}", pid, n);
        let fd = unsafe {
            libc::open(util::cstr(&p).as_ptr(),
                       libc::O_RDONLY | libc::O_CLOEXEC)
        };
        if fd < 0 {
            eprintln!("打不开 {}: {}", p, util::errno_text());
            return 1;
        }
        fds.push(fd);
    }
    for (i, fd) in fds.iter().enumerate() {
        if unsafe { libc::setns(*fd, 0) } != 0 {
            eprintln!("setns {} 失败: {}", names[i], util::errno_text());
            return 1;
        }
    }
    // 切到容器根（mount ns 已经换过来了）
    if std::env::set_current_dir("/").is_err() {
        eprintln!("chdir / 失败: {}", util::errno_text());
        return 1;
    }
    let p = unsafe { libc::fork() };
    if p < 0 {
        eprintln!("fork 失败: {}", util::errno_text());
        return 1;
    }
    if p == 0 {
        util::strip_preload();
        // 容器里的工具在 /usr/bin 等，宿主 PATH 里没有，补上
        if Path::new("/usr/bin").is_dir() {
            let cur = std::env::var("PATH").unwrap_or_default();
            let p1 = "/usr/local/sbin:/usr/local/bin:/usr/sbin";
            let p2 = "/usr/bin:/sbin:/bin";
            let base = format!("{}:{}", p1, p2);
            std::env::set_var("PATH", format!("{}:{}", base, cur));
            if !Path::new("/home").exists() {
                std::env::set_var("HOME", "/root");
            }
        }
        let mut args: Vec<std::ffi::CString> = Vec::new();
        if argv.is_empty() {
            if Path::new("/bin/bash").exists() {
                args.push(util::cstr("/bin/bash"));
                args.push(util::cstr("-il"));
            } else {
                args.push(util::cstr("/bin/sh"));
                args.push(util::cstr("-i"));
            }
        } else {
            for a in argv {
                args.push(util::cstr(a));
            }
        }
        let mut ap: Vec<*const libc::c_char> =
            args.iter().map(|x| x.as_ptr()).collect();
        ap.push(std::ptr::null());
        let prog = args[0].to_string_lossy().into_owned();
        let cp = util::cstr(&prog);
        // execvp 会按 PATH 搜索（execv 不会，用户可能给裸命令名）
        unsafe { libc::execvp(cp.as_ptr(), ap.as_ptr()) };
        eprintln!("exec {} 失败: {}", prog, util::errno_text());
        unsafe { libc::_exit(127) }
    }
    let mut st = 0;
    unsafe { libc::waitpid(p, &mut st, 0) };
    if (st & 0x7f) == 0 { (st >> 8) & 0xff } else { 128 + (st & 0x7f) }
}
