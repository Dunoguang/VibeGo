//! start：启动容器（namespace + 挂载 + pivot_root + exec systemd）。
//! 父进程（supervisor）留在宿主 ns 只做监控，所有挂载都在私有 mount ns 内。
use crate::config::Container;
use crate::prepare;
use crate::util;
use std::io::Write;
use std::os::unix::io::{AsRawFd, RawFd};

macro_rules! step {
    ($log:expr, $name:expr, $e:expr) => {{
        match $e {
            Ok(_) => {
                if let Some(l) = $log.as_mut() {
                    let _ = writeln!(l, "  ok   {}", $name);
                }
            }
            Err(err) => {
                if let Some(l) = $log.as_mut() {
                    let _ = writeln!(l, "  FAIL {}: {}", $name, err);
                }
                unsafe { libc::_exit(1) }
            }
        }
    }};
}

pub fn start(c: &Container, detach: bool, shell: bool) -> i32 {
    if c.running() {
        eprintln!("容器 {} 已在运行（pid {}）", c.name,
                  c.read_pid().unwrap_or(0));
        return 1;
    }
    let rootfs = c.rootfs();
    if !std::path::Path::new(&rootfs).is_dir() {
        eprintln!("rootfs 不存在: {}（先用 vibego new 创建）", rootfs);
        return 1;
    }
    let prog = if shell {
        "/bin/sh".to_string()
    } else {
        c.cmd.clone()
    };
    let bin = format!("{}{}", rootfs, prog);
    if !std::path::Path::new(&bin).exists() {
        eprintln!("容器里找不到 {}（rootfs 里没有 systemd？）", bin);
        return 1;
    }
    for d in ["log", "run", "tmp"] {
        let _ = std::fs::create_dir_all(format!("{}/{}", c.path, d));
    }
    let logpath = c.log_file();
    if detach {
        detach_start(c, &prog, &logpath, shell)
    } else {
        launch(c, &prog, None, shell)
    }
}

fn open_log(path: &str) -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

fn detach_start(c: &Container, prog: &str, logpath: &str, shell: bool)
    -> i32
{
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        eprintln!("pipe 失败: {}", util::errno_text());
        return 1;
    }
    let (rfd, wfd) = (fds[0], fds[1]);
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        eprintln!("fork 失败: {}", util::errno_text());
        return 1;
    }
    if pid == 0 {
        unsafe {
            libc::close(rfd);
            libc::setsid();
            let dn = util::cstr("/dev/null");
            let fd = libc::open(dn.as_ptr(), libc::O_RDWR);
            if fd >= 0 {
                libc::dup2(fd, 0);
                libc::dup2(fd, 1);
                libc::dup2(fd, 2);
                if fd > 2 {
                    libc::close(fd);
                }
            }
        }
        let code = launch(c, prog, Some(wfd), shell);
        unsafe { libc::_exit(code) }
    }
    unsafe { libc::close(wfd) };
    let mut buf = [0u8; 64];
    let n = unsafe {
        libc::read(rfd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
    };
    unsafe { libc::close(rfd) };
    if n > 0 {
        let s = String::from_utf8_lossy(&buf[..n as usize])
            .trim().to_string();
        println!("容器 {} 已启动：宿主侧 PID1 = {}，日志 {}", c.name, s,
                 logpath);
        0
    } else {
        eprintln!("容器 {} 启动失败，看日志：{}", c.name, logpath);
        1
    }
}

fn block_signals() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGCHLD] {
            libc::sigaddset(&mut set, s);
        }
        libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

fn launch(c: &Container, prog: &str, notify: Option<RawFd>, shell: bool)
    -> i32
{
    let rootfs = c.rootfs();
    let mut log = open_log(&c.log_file());
    if let Some(l) = log.as_mut() {
        let _ = writeln!(l, "\n=== vibego start {} ({}) ===",
                         c.name,
                         if shell { "shell 模式" } else { "systemd" });
    }
    block_signals();
    // 最小隔离集：PID + Mount + UTS + Cgroup
    // - UTS 保留：不然 systemd 的 sethostname() 会改掉宿主 hostname
    // - IPC 不要：systemd 不依赖；容器内 IPC 与宿主共享无影响
    let flags = libc::CLONE_NEWNS
        | libc::CLONE_NEWPID
        | libc::CLONE_NEWUTS
        | libc::CLONE_NEWCGROUP;
    if unsafe { libc::unshare(flags) } != 0 {
        let e = util::errno_text();
        if let Some(l) = log.as_mut() {
            let _ = writeln!(l, "unshare 失败: {}", e);
        }
        return 1;
    }
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return 1;
    }
    if pid == 0 {
        let fd = log.as_ref().map(|f| f.as_raw_fd()).unwrap_or(-1);
        if fd >= 0 && !shell {
            unsafe {
                libc::dup2(fd, 1);
                libc::dup2(fd, 2);
            }
        }
        child_main(c, &rootfs, prog, &mut log, shell)
    }

    let stt = crate::config::starttime_of(pid).unwrap_or(0);
    if let Err(e) = std::fs::write(c.pid_file(),
                                  format!("{} {}\n", pid, stt)) {
        if let Some(l) = log.as_mut() {
            let _ = writeln!(l, "write pid failed: {}", e);
        }
    }
    if let Some(w) = notify {
        let s = format!("{}", pid);
        unsafe {
            libc::write(w, s.as_ptr() as *const libc::c_void, s.len());
            libc::close(w);
        }
    }
    let code = supervise(pid, &mut log);
    let _ = std::fs::remove_file(c.pid_file());
    if let Some(l) = log.as_mut() {
        let _ = writeln!(l, "=== 容器 {} 结束，exit={} ===", c.name, code);
    }
    code
}

fn decode(st: libc::c_int) -> i32 {
    if (st & 0x7f) == 0 {
        (st >> 8) & 0xff
    } else {
        128 + (st & 0x7f)
    }
}

fn supervise(pid: libc::pid_t, log: &mut Option<std::fs::File>) -> i32 {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGCHLD] {
            libc::sigaddset(&mut set, s);
        }
        let fd = libc::signalfd(-1, &set, libc::SFD_CLOEXEC);
        if fd < 0 {
            let mut st = 0;
            libc::waitpid(pid, &mut st, 0);
            return decode(st);
        }
        let sz = std::mem::size_of::<libc::signalfd_siginfo>();
        let mut sent = false;
        let mut t0 = std::time::Instant::now();
        loop {
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let pr = libc::poll(&mut pfd, 1, 1000);
            if pr > 0 {
                let mut si: libc::signalfd_siginfo = std::mem::zeroed();
                let n = libc::read(fd,
                                   &mut si as *mut _ as *mut libc::c_void,
                                   sz);
                if n as usize == sz {
                    let sig = si.ssi_signo as libc::c_int;
                    if sig == libc::SIGCHLD {
                        let mut st = 0;
                        if libc::waitpid(pid, &mut st, 0) == pid {
                            return decode(st);
                        }
                    } else if !sent {
                        sent = true;
                        t0 = std::time::Instant::now();
                        if let Some(l) = log.as_mut() {
                            let _ = writeln!(l, "收到信号 {} -> 转发 SIGTERM",
                                             sig);
                        }
                        libc::kill(pid, libc::SIGTERM);
                    }
                }
            }
            if sent && t0.elapsed().as_secs() >= 15 {
                if let Some(l) = log.as_mut() {
                    let _ = writeln!(l, "超时 15s -> SIGKILL");
                }
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

/// 容器 /dev 是从宿主 bind 来的，带着宿主的挂载（cgroup v1 的
/// /dev/cpuset /dev/memcg、binderfs、usb-ffs ...），容器内 root 能写
/// 它们会影响宿主。在私有 mount ns 内全部卸掉（对外面无影响），
/// 之后我们再挂自己的 /dev/pts /dev/shm /dev/mqueue。
/// 注意：必须在 pivot_root 之后调用，路径才是容器视角的 /dev/*。
fn hide_dev_mounts(log: &mut Option<std::fs::File>) {
    let mi = std::fs::read_to_string("/proc/self/mountinfo")
        .unwrap_or_default();
    let mut n = 0;
    for line in mi.lines() {
        let p: Vec<&str> = line.splitn(2, " - ").collect();
        if p.len() != 2 {
            continue;
        }
        let mp = p[0].split_whitespace().nth(4).unwrap_or("");
        if !mp.starts_with("/dev/") {
            continue;
        }
        let c = util::cstr(mp);
        if unsafe { libc::umount2(c.as_ptr(), libc::MNT_DETACH) } == 0 {
            n += 1;
        }
    }
    if let Some(l) = log.as_mut() {
        let _ = writeln!(l, "  ok   hide host /dev mounts: {}", n);
    }
}

fn child_main(c: &Container, rootfs: &str, prog: &str,
              log: &mut Option<std::fs::File>, shell: bool) -> ! {
    if let Some(l) = log.as_mut() {
        let _ = writeln!(l, "-- 容器 {} 内准备（本进程是新 PID ns 的 PID 1）",
                         c.name);
    }
    unsafe {
        let mut e: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut e);
        libc::sigprocmask(libc::SIG_SETMASK, &e, std::ptr::null_mut());
    }
    step!(log, "MS_REC|MS_PRIVATE /", mount_priv());
    step!(log, "bind rootfs self (bind mount)",
          mnt(rootfs, rootfs, "", libc::MS_BIND | libc::MS_REC, None));
    step!(log, "mkdir rootfs/dev", mkdir_p(&format!("{}/dev", rootfs)));
    step!(log, "bind /dev 进容器",
          mnt("/dev", &format!("{}/dev", rootfs), "",
              libc::MS_BIND | libc::MS_REC, None));
    if c.host_data {
        step!(log, "mkdir rootfs/mnt/host-data",
              mkdir_p(&format!("{}/mnt/host-data", rootfs)));
        step!(log, "bind /data -> /mnt/host-data",
              mnt("/data", &format!("{}/mnt/host-data", rootfs), "",
                  libc::MS_BIND | libc::MS_REC, None));
    }
    step!(log, "pivot_root", pivot(rootfs));
    step!(log, "mkdir /proc", mkdir_p("/proc"));
    step!(log, "mount proc",
          mnt("proc", "/proc", "proc",
              libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC, None));
    // 必须在 /proc 重挂之后（否则读不到 mountinfo）
    hide_dev_mounts(log);
    step!(log, "mkdir /sys", mkdir_p("/sys"));
    step!(log, "mount sysfs",
          mnt("sysfs", "/sys", "sysfs",
              libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC, None));
    step!(log, "mkdir /dev/pts", mkdir_p("/dev/pts"));
    step!(log, "mount devpts(newinstance)",
          mnt("devpts", "/dev/pts", "devpts",
              libc::MS_NOSUID | libc::MS_NOEXEC,
              Some("newinstance,ptmxmode=0666,mode=0620,gid=5")));
    step!(log, "mkdir /dev/shm", mkdir_p("/dev/shm"));
    step!(log, "mount tmpfs /dev/shm",
          mnt("tmpfs", "/dev/shm", "tmpfs",
              libc::MS_NOSUID | libc::MS_NODEV,
              Some("mode=1777,size=64m")));
    step!(log, "mkdir /run", mkdir_p("/run"));
    step!(log, "mount tmpfs /run",
          mnt("tmpfs", "/run", "tmpfs",
              libc::MS_NOSUID | libc::MS_NODEV,
              Some("mode=755,size=64m")));
    step!(log, "mkdir /tmp", mkdir_p("/tmp"));
    step!(log, "mount tmpfs /tmp",
          mnt("tmpfs", "/tmp", "tmpfs",
              libc::MS_NOSUID | libc::MS_NODEV,
              Some("mode=1777,size=256m")));
    step!(log, "mkdir /sys/fs/cgroup", mkdir_p("/sys/fs/cgroup"));
    step!(log, "mount tmpfs /sys/fs/cgroup",
          mnt("tmpfs", "/sys/fs/cgroup", "tmpfs",
              libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
              Some("mode=755,size=16m")));
    let cg2 = mnt("cgroup2", "/sys/fs/cgroup", "cgroup2",
                  libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                  Some("nsdelegate"));
    if cg2.is_err() {
        let e1 = cg2.err().unwrap_or_default();
        let c2 = mnt("cgroup2", "/sys/fs/cgroup", "cgroup2",
                     libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                     None);
        if let Some(l) = log.as_mut() {
            match c2 {
                Ok(_) => {
                    let _ = writeln!(l,
                        "  warn cgroup2: nsdelegate 失败({})，无选项 OK", e1);
                }
                Err(e2) => {
                    let _ = writeln!(l,
                        "  warn cgroup2 挂不上({} / {})，留空目录", e1, e2);
                }
            }
        }
    } else if let Some(l) = log.as_mut() {
        let _ = writeln!(l, "  ok   mount cgroup2(nsdelegate)");
    }
    match std::fs::create_dir_all("/run/systemd")
        .map_err(|e| format!("{}", e))
        .and_then(|_| std::fs::write("/run/systemd/container", "vibego\n")
                  .map_err(|e| format!("{}", e))) {
        Ok(_) => {}
        Err(e) => {
            if let Some(l) = log.as_mut() {
                let _ = writeln!(l, "  warn /run/systemd/container: {}", e);
            }
        }
    }
    match log.as_mut() {
        Some(f) => prepare::prepare("/", f),
        None => {
            if let Ok(mut dn) = std::fs::OpenOptions::new()
                .write(true).open("/dev/null")
            {
                prepare::prepare("/", &mut dn);
            }
        }
    }
    exec_final(prog, log, shell)
}

fn mount_priv() -> Result<(), String> {
    let c = util::cstr("/");
    let r = unsafe {
        libc::mount(std::ptr::null(), c.as_ptr(), std::ptr::null(),
                    libc::MS_REC | libc::MS_PRIVATE, std::ptr::null())
    };
    if r != 0 {
        return Err(util::errno_text());
    }
    Ok(())
}

fn mnt(src: &str, tgt: &str, fstype: &str, flags: libc::c_ulong,
       data: Option<&str>) -> Result<(), String> {
    let csrc = util::cstr(src);
    let ctgt = util::cstr(tgt);
    let cfs = util::cstr(fstype);
    let cdata = data.map(util::cstr);
    let dp = match &cdata {
        Some(c) => c.as_ptr() as *const libc::c_void,
        None => std::ptr::null(),
    };
    let r = unsafe {
        libc::mount(csrc.as_ptr(), ctgt.as_ptr(), cfs.as_ptr(), flags, dp)
    };
    if r != 0 {
        return Err(util::errno_text());
    }
    Ok(())
}

fn mkdir_p(p: &str) -> Result<(), String> {
    std::fs::create_dir_all(p).map_err(|e| format!("{}", e))
}

fn pivot(rootfs: &str) -> Result<(), String> {
    std::env::set_current_dir(rootfs).map_err(|e| format!("{}", e))?;
    let old = ".vibego-oldroot";
    std::fs::create_dir_all(old).map_err(|e| format!("{}", e))?;
    let c_new = util::cstr(".");
    let c_old = util::cstr(old);
    let pr = unsafe {
        libc::syscall(libc::SYS_pivot_root, c_new.as_ptr(),
                     c_old.as_ptr())
    };
    if pr != 0 {
        return Err(util::errno_text());
    }
    std::env::set_current_dir("/").map_err(|e| format!("{}", e))?;
    let c_abs = util::cstr("/.vibego-oldroot");
    unsafe {
        libc::umount2(c_abs.as_ptr(), libc::MNT_DETACH);
    }
    let _ = std::fs::remove_dir("/.vibego-oldroot");
    Ok(())
}

fn exec_final(prog: &str, log: &mut Option<std::fs::File>, shell: bool)
    -> !
{
    util::strip_preload();
    // 清空后只加必要项（零继承）：宿主 PATH=/system/bin、
    // BOOTCLASSPATH、MY_*_ROOT、TMPDIR 等一个都不进容器
    let mut env: Vec<std::ffi::CString> = Vec::new();
    for (k, v) in util::container_env(None) {
        env.push(util::cstr(&format!("{}={}", k, v)));
    }
    let mut args: Vec<std::ffi::CString> = Vec::new();
    args.push(util::cstr(prog));
    let is_systemd = prog.ends_with("systemd");
    if shell {
        args.push(util::cstr("-i"));
    } else if is_systemd {
        let kmsg = std::path::Path::new("/dev/kmsg").exists();
        args.push(util::cstr("--system"));
        args.push(util::cstr(if kmsg {
            "--log-target=kmsg"
        } else {
            "--log-target=console"
        }));
        args.push(util::cstr("--log-level=info"));
    }
    let mut a: Vec<*const libc::c_char> =
        args.iter().map(|c| c.as_ptr()).collect();
    a.push(std::ptr::null());
    let mut e: Vec<*const libc::c_char> =
        env.iter().map(|c| c.as_ptr()).collect();
    e.push(std::ptr::null());
    if let Some(l) = log.as_mut() {
        let _ = writeln!(l, "  exec {}", prog);
        let _ = l.flush();
    }
    let c = util::cstr(prog);
    unsafe {
        libc::execve(c.as_ptr(), a.as_ptr(), e.as_ptr());
    }
    if let Some(l) = log.as_mut() {
        let _ = writeln!(l, "  FAIL execve: {}", util::errno_text());
    }
    unsafe { libc::_exit(127) }
}
