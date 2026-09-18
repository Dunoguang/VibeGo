//! new：创建容器（解包 + 准备基础文件 + 写 config + 注册）
use crate::config::{self, Container};
use crate::prepare;
use std::io::Write;

pub fn new(base: &str, name: &str, path: Option<&str>, source: &str,
           host_data: bool) -> i32 {
    if let Err(e) = config::validate_name(name) {
        eprintln!("{}", e);
        return 2;
    }
    if source.is_empty() {
        eprintln!("需要 --source（tarball 或目录）");
        return 2;
    }
    if !std::path::Path::new(source).exists() {
        eprintln!("source 不存在: {}", source);
        return 2;
    }
    if let Ok(c) = config::resolve(base, name) {
        eprintln!("容器 {} 已存在（{}），先 vibego rm {}", name, c.path,
                  name);
        return 1;
    }
    let cpath = match path {
        Some(p) => p.to_string(),
        None => format!("{}/{}", base, name),
    };
    if std::path::Path::new(&format!("{}/rootfs", cpath)).exists() {
        eprintln!("{} 下已经有 rootfs 了，换个 --path 或先清理", cpath);
        return 1;
    }
    for d in ["rootfs", "log", "run"] {
        if let Err(e) = std::fs::create_dir_all(format!("{}/{}", cpath, d))
        {
            eprintln!("建目录失败 {}: {}", d, e);
            return 1;
        }
    }
    let logpath = format!("{}/log/vibego.log", cpath);
    let mut log = match std::fs::OpenOptions::new()
        .create(true).append(true).open(&logpath)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("日志打不开 {}: {}", logpath, e);
            return 1;
        }
    };
    let _ = writeln!(log, "=== vibego new {} ===", name);
    println!("创建容器 {}：{}", name, cpath);
    println!("解包 {} ...（大 tarball 要等一会）", source);
    if let Err(e) = prepare::unpack(source, &format!("{}/rootfs", cpath),
                                    &mut log) {
        eprintln!("解包失败: {}", e);
        let _ = writeln!(log, "解包失败: {}", e);
        return 1;
    }
    let rootfs = format!("{}/rootfs", cpath);
    let has_systemd = std::path::Path::new(
        &format!("{}/usr/lib/systemd/systemd", rootfs)).exists();
    if !has_systemd && !std::path::Path::new(
        &format!("{}/sbin/init", rootfs)).exists()
    {
        println!("警告：rootfs 里没找到 systemd，start 可能失败");
    }
    prepare::prepare(&rootfs, &mut log);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let c = Container {
        name: name.to_string(),
        path: cpath.clone(),
        source: source.to_string(),
        created: now,
        host_data,
        cmd: if has_systemd {
            "/usr/lib/systemd/systemd".to_string()
        } else {
            "/bin/sh".to_string()
        },
    };
    if let Err(e) = config::save(&c) {
        eprintln!("写 config.json 失败: {}", e);
        return 1;
    }
    if let Err(e) = config::register(base, name, &cpath) {
        eprintln!("写注册表失败: {}", e);
        return 1;
    }
    println!("完成：vibego start {} / vibego enter {}", name, name);
    0
}
