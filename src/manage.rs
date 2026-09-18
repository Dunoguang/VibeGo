//! stop / list / logs / rm
use crate::config::{self, Container};
use crate::util;
use std::io::{Read, Write};

pub fn stop(c: &Container, timeout: u64, graceful: bool) -> i32 {
    if !c.running() {
        println!("容器 {} 没在运行", c.name);
        let _ = std::fs::remove_file(c.pid_file());
        return 0;
    }
    let pid = c.read_pid().unwrap_or(0);
    // 优雅路径：容器内有 systemd 就用 systemctl exit —— 这是 systemd
    // 给容器用的命令，走正常关机流程后让 PID1 退出，不会调内核 reboot()
    if graceful {
        let sc = format!("{}/usr/bin/systemctl", c.rootfs());
        if std::path::Path::new(&sc).exists() {
            println!("优雅关机：容器内 systemctl exit 0");
            let args = vec![
                "/usr/bin/systemctl".to_string(),
                "--no-block".to_string(),
                "exit".to_string(),
                "0".to_string(),
            ];
            let _ = crate::enter::enter(c, &args);
        } else {
            println!("容器里没有 systemctl，跳过优雅关机");
        }
        if wait_gone(c, timeout) {
            println!("已优雅停止");
            let _ = std::fs::remove_file(c.pid_file());
            return 0;
        }
        println!("优雅关机超时 {}s，转常规停止", timeout);
    }
    println!("停止容器 {}（pid {}）...", c.name, pid);
    unsafe { libc::kill(pid, libc::SIGTERM) };
    if wait_gone(c, 5) {
        println!("已停止（SIGTERM）");
        let _ = std::fs::remove_file(c.pid_file());
        return 0;
    }
    println!("SIGTERM 无效（PID1 忽略），改 SIGKILL");
    unsafe { libc::kill(pid, libc::SIGKILL) };
    let _ = wait_gone(c, 3);
    let _ = std::fs::remove_file(c.pid_file());
    0
}

/// 等容器退出，返回是否已退出
fn wait_gone(c: &Container, secs: u64) -> bool {
    let t0 = std::time::Instant::now();
    while t0.elapsed().as_secs() < secs {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if !c.running() {
            return true;
        }
    }
    !c.running()
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
