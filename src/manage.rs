//! stop / list / logs / rm
use crate::config::{self, Container};
use crate::util;
use std::io::{Read, Write};

pub fn stop(c: &Container, timeout: u64) -> i32 {
    if !c.running() {
        println!("容器 {} 没在运行", c.name);
        let _ = std::fs::remove_file(c.pid_file());
        return 0;
    }
    let pid = c.read_pid().unwrap_or(0);
    println!("停止容器 {}（pid {}）...", c.name, pid);
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let t0 = std::time::Instant::now();
    while t0.elapsed().as_secs() < timeout {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if !c.running() {
            println!("已停止");
            let _ = std::fs::remove_file(c.pid_file());
            return 0;
        }
    }
    println!("超时 {}s，SIGKILL", timeout);
    unsafe { libc::kill(pid, libc::SIGKILL) };
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = std::fs::remove_file(c.pid_file());
    0
}

pub fn list(base: &str) -> i32 {
    let v = config::list_all(base);
    if v.is_empty() {
        println!("（没有容器。vibego new --name X --source y.tar.gz）");
        return 0;
    }
    println!("{:<18} {:<8} {:<12} {}", "NAME", "STATUS", "UPTIME", "PATH");
    for c in v {
        let st = if c.running() { "running" } else { "stopped" };
        println!("{:<18} {:<8} {:<12} {}", c.name, st, c.uptime_str(),
                 c.path);
    }
    0
}

pub fn logs(c: &Container, follow: bool, lines: usize) -> i32 {
    let p = c.log_file();
    let mut f = match std::fs::File::open(&p) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("打不开 {}: {}", p, e);
            return 1;
        }
    };
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    let all: Vec<&str> = s.lines().collect();
    let start = if all.len() > lines { all.len() - lines } else { 0 };
    for l in &all[start..] {
        println!("{}", l);
    }
    if !follow {
        return 0;
    }
    // 简单 tail -f：轮询文件增量
    loop {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut buf = String::new();
        if f.read_to_string(&mut buf).is_ok() && !buf.is_empty() {
            print!("{}", buf);
            let _ = std::io::stdout().flush();
        }
    }
}

pub fn rm(base: &str, c: &Container, force: bool) -> i32 {
    if c.running() {
        if !force {
            eprintln!("容器 {} 正在运行，先 vibego stop {} 或加 --force",
                      c.name, c.name);
            return 1;
        }
        let pid = c.read_pid().unwrap_or(0);
        unsafe { libc::kill(pid, libc::SIGTERM) };
        std::thread::sleep(std::time::Duration::from_millis(800));
        if c.running() {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
    }
    println!("删除 {} ...", c.path);
    if let Err(e) = std::fs::remove_dir_all(&c.path) {
        eprintln!("删不掉 {}: {}（有挂载残留？）", c.path, e);
        return 1;
    }
    if let Err(e) = config::unregister(base, &c.name) {
        eprintln!("注册表更新失败: {}", e);
        return 1;
    }
    println!("已删除容器 {}", c.name);
    let _ = util::errno_text();
    0
}
