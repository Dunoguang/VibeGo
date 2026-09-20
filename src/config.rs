//! 容器配置 + 注册表（containers.conf）
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_BASE: &str = "/data/VibeGo";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Container {
    pub name: String,
    pub path: String,
    pub source: String,
    pub created: u64,
    pub host_data: bool,
    pub cmd: String,
    #[serde(default = "default_true")]
    pub autostart: bool,
}

fn default_true() -> bool {
    true
}

impl Container {
    pub fn rootfs(&self) -> String {
        format!("{}/rootfs", self.path)
    }
    pub fn config_file(&self) -> String {
        format!("{}/config.json", self.path)
    }
    pub fn log_file(&self) -> String {
        format!("{}/log/vibego.log", self.path)
    }
    pub fn pid_file(&self) -> String {
        format!("{}/run/vibego.pid", self.path)
    }
    pub fn read_pid(&self) -> Option<i32> {
        let s = std::fs::read_to_string(self.pid_file()).ok()?;
        s.split_whitespace().next()?.parse::<i32>().ok()
    }

    fn pid_starttime(&self) -> Option<u64> {
        let s = std::fs::read_to_string(self.pid_file()).ok()?;
        let mut it = s.split_whitespace();
        it.next()?;
        it.next().and_then(|x| x.parse::<u64>().ok())
    }
    /// pid 文件里的进程是否真的还活着（顺带防 PID 复用）
    pub fn running(&self) -> bool {
        let pid = match self.read_pid() {
            Some(p) => p,
            None => return false,
        };
        if pid <= 1 {
            return false;
        }
        if unsafe { libc::kill(pid, 0) } != 0 {
            return false;
        }
        let want = match self.pid_starttime() {
            Some(t) => t,
            None => return true,
        };
        if want == 0 {
            return true;
        }
        starttime_of(pid).map(|cur| cur == want).unwrap_or(false)
    }
    pub fn uptime_str(&self) -> String {
        if !self.running() {
            return "-".into();
        }
        let p = match self.read_pid() {
            Some(p) => p,
            None => return "-".into(),
        };
        let ticks: u64 = starttime_of(p).unwrap_or(0);
        let up = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split(' ').next()
                      .and_then(|x| x.parse::<f64>().ok()))
            .unwrap_or(0.0);
        let secs = up - (ticks as f64 / 100.0);
        if secs < 0.0 {
            return "0s".into();
        }
        let s = secs as u64;
        format!("{}h{}m{}s", s / 3600, (s % 3600) / 60, s % 60)
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct Registry {
    pub version: u32,
    pub containers: BTreeMap<String, String>,
}

/// /proc/<pid>/stat 第 22 字段（starttime，ticks）用于防 PID 复用
pub fn starttime_of(pid: i32) -> Option<u64> {
    let st = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    let after = st.rsplit(')').next()?;
    after.split_whitespace().nth(19).and_then(|x| x.parse::<u64>().ok())
}

pub fn registry_file(base: &str) -> String {
    format!("{}/containers.conf", base)
}

pub fn load_registry(base: &str) -> Registry {
    match std::fs::read_to_string(registry_file(base)) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Registry::default(),
    }
}

pub fn save_registry(base: &str, r: &Registry) -> Result<(), String> {
    std::fs::create_dir_all(base).map_err(|e| format!("{}", e))?;
    let s = serde_json::to_string_pretty(r).map_err(|e| format!("{}", e))?;
    std::fs::write(registry_file(base), s).map_err(|e| format!("{}", e))
}

pub fn register(base: &str, name: &str, path: &str) -> Result<(), String> {
    let mut r = load_registry(base);
    r.version = 1;
    r.containers.insert(name.to_string(), path.to_string());
    save_registry(base, &r)
}

pub fn unregister(base: &str, name: &str) -> Result<(), String> {
    let mut r = load_registry(base);
    r.containers.remove(name);
    save_registry(base, &r)
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("名字不能为空".into());
    }
    if name.len() > 64 {
        return Err("名字太长（>64）".into());
    }
    if name.starts_with('.') {
        return Err("名字不能以 . 开头".into());
    }
    for c in name.chars() {
        let ok = c.is_ascii_alphanumeric()
            || c == '_' || c == '-' || c == '.';
        if !ok {
            return Err(format!(
                "名字含非法字符 '{}'，只允许 [A-Za-z0-9_.-]", c));
        }
    }
    Ok(())
}

pub fn save(c: &Container) -> Result<(), String> {
    let s = serde_json::to_string_pretty(c).map_err(|e| format!("{}", e))?;
    std::fs::write(c.config_file(), s).map_err(|e| format!("{}", e))
}

pub fn read_config(dir: &str) -> Result<Container, String> {
    let f = format!("{}/config.json", dir);
    let s = std::fs::read_to_string(&f).map_err(|e| format!("{}: {}", f, e))?;
    serde_json::from_str(&s).map_err(|e| format!("{} 解析失败: {}", f, e))
}

/// 按名解析：先查注册表，回退到 <base>/<name>/config.json
pub fn resolve(base: &str, name: &str) -> Result<Container, String> {
    validate_name(name)?;
    let reg = load_registry(base);
    if let Some(p) = reg.containers.get(name) {
        if let Ok(c) = read_config(p) {
            return Ok(c);
        }
    }
    let p = format!("{}/{}", base, name);
    read_config(&p).map_err(|_| {
        format!("找不到容器 {}（注册表和 {} 下都没有）", name, p)
    })
}

pub fn list_all(base: &str) -> Vec<Container> {
    let reg = load_registry(base);
    let mut v: Vec<Container> = Vec::new();
    for p in reg.containers.values() {
        if let Ok(c) = read_config(p) {
            v.push(c);
        }
    }
    if let Ok(rd) = std::fs::read_dir(base) {
        for e in rd.flatten() {
            let d = e.path().to_string_lossy().into_owned();
            if !std::path::Path::new(&format!("{}/rootfs", d)).is_dir() {
                continue;
            }
            if let Ok(c) = read_config(&d) {
                if !v.iter().any(|x| x.name == c.name) {
                    v.push(c);
                }
            }
        }
    }
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}
