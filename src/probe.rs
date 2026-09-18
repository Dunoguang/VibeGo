//! M1：能力探测。只读；所有会改状态的操作都在 fork 出的子进程里。
use crate::util::{self, run_in_child};
use crate::Opts;

#[derive(Clone, Copy)]
enum St { Pass, Fail, Warn, Info }

struct Rep { fails: usize, warns: usize }

impl Rep {
    fn new() -> Self { Rep { fails: 0, warns: 0 } }

    fn section(&mut self, t: &str) { println!("\n== {} ==", t); }

    fn of(&mut self, st: St, name: &str, detail: &str) {
        let tag = match st {
            St::Pass => "PASS",
            St::Fail => "FAIL",
            St::Warn => "WARN",
            St::Info => "INFO",
        };
        match st {
            St::Fail => self.fails += 1,
            St::Warn => self.warns += 1,
            _ => {}
        }
        println!("  {:<4} {:<26} {}", tag, name, detail);
    }

    fn absorb(&mut self, out: &str) {
        for line in out.lines() {
            let f: Vec<&str> = line.split('\u{1}').collect();
            if f.len() != 3 {
                if !line.trim().is_empty() {
                    self.of(St::Info, "(子进程)", line);
                }
                continue;
            }
            let st = match f[0] {
                "PASS" => St::Pass,
                "FAIL" => St::Fail,
                "WARN" => St::Warn,
                _ => St::Info,
            };
            self.of(st, f[1], f[2]);
        }
    }
}

pub fn run(o: &Opts) -> i32 {
    let mut r = Rep::new();
    let euid = unsafe { libc::geteuid() };
    let root = euid == 0;

    r.section("0. 身份与环境");
    r.of(if root { St::Pass } else { St::Fail }, "euid",
         &format!("{}{}", euid,
                  if root { "" } else { "  <- 需要 root" }));
    r.of(St::Info, "内核",
         &util::read_trim("/proc/sys/kernel/osrelease")
             .unwrap_or_else(|| "?".into()));
    r.of(St::Info, "SELinux 域",
         &util::read_trim("/proc/self/attr/current")
             .unwrap_or_else(|| "(无)".into()));
    if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
        r.of(St::Info, "CapEff",
             util::grep_line(&s, "CapEff").unwrap_or_default().trim());
        r.of(St::Info, "Seccomp",
             util::grep_line(&s, "Seccomp:").unwrap_or_default().trim());
    }
    if let Some(cg) = util::read_trim("/proc/self/cgroup") {
        let j = cg.split('\n').collect::<Vec<_>>().join(" | ");
        r.of(St::Info, "/proc/self/cgroup", &j);
    }
    if let Ok(cl) = std::fs::read_to_string("/proc/cmdline") {
        let t: Vec<&str> = cl
            .split_whitespace()
            .filter(|x| x.contains("cgroup"))
            .collect();
        r.of(St::Info, "cmdline(cgroup*)",
             &if t.is_empty() { "(无)".into() } else { t.join(" ") });
    }
    if let Ok(fs) = std::fs::read_to_string("/proc/filesystems") {
        let has = |n: &str| fs.lines().any(|l| l.ends_with(n));
        r.of(St::Info, "/proc/filesystems",
             &format!("cgroup2={} tmpfs={} proc={} sysfs={} devpts={}",
                      has("cgroup2"), has("tmpfs"),
                      has("proc"), has("sysfs"), has("devpts")));
    }

    // systemd 的日志通道（实测：只重定向 stdout/stderr 看不到任何东西）
    r.section("1. 日志通道");
    let kmsg = std::path::Path::new("/dev/kmsg").exists();
    let kmsg_msg = if kmsg {
        "存在：--log-target=kmsg 可用".to_string()
    } else {
        "不存在：kmsg 日志会静默，需 mknod c 1 11".to_string()
    };
    r.of(if kmsg { St::Pass } else { St::Warn }, "/dev/kmsg",
         &kmsg_msg);
    let con = std::path::Path::new("/dev/console").exists();
    r.of(if con { St::Info } else { St::Warn }, "/dev/console",
         if con { "存在" } else { "不存在" });
    if let Ok(mi) = std::fs::read_to_string("/proc/self/mountinfo") {
        let n = mi.lines().filter(|l| l.contains(" - cgroup")).count();
        r.of(if n > 0 { St::Pass } else { St::Warn },
             "现有 cgroup 挂载", &format!("{} 条", n));
    }

    // namespace
    r.section("2. namespace（每项在独立子进程里 unshare）");
    if !root {
        r.of(St::Warn, "全部跳过", "非 root");
    } else {
        let cases: [(&str, libc::c_int, bool); 7] = [
            ("PID", libc::CLONE_NEWPID, true),
            ("Mount", libc::CLONE_NEWNS, true),
            ("UTS", libc::CLONE_NEWUTS, true),
            ("IPC", libc::CLONE_NEWIPC, true),
            ("Cgroup", libc::CLONE_NEWCGROUP, true),
            ("Net", libc::CLONE_NEWNET, false),
            ("User", libc::CLONE_NEWUSER, false),
        ];
        for (name, flag, need) in cases {
            let (_, out) = run_in_child(move || {
                if unsafe { libc::unshare(flag) } != 0 {
                    util::kv("FAIL", name,
                             &format!("unshare 失败 {}", util::errno_text()));
                    return;
                }
                if flag == libc::CLONE_NEWPID {
                    let pid = unsafe { libc::fork() };
                    if pid == 0 {
                        let p = unsafe { libc::getpid() };
                        if p == 1 {
                            util::kv("PASS", name,
                                     "OK：fork 后 getpid()=1");
                        } else {
                            util::kv("FAIL", name,
                                     &format!("fork 后 pid={} 期望 1", p));
                        }
                        unsafe { libc::_exit(0) }
                    }
                    let mut st = 0;
                    unsafe { libc::waitpid(pid, &mut st, 0) };
                    return;
                }
                util::kv("PASS", name,
                         if need { "OK" } else { "OK（不用，参考）" });
            });
            if out.trim().is_empty() {
                let st = if need { St::Fail } else { St::Warn };
                r.of(st, name, "子进程无输出（可能被 SELinux 拒）");
            } else {
                r.absorb(&out);
            }
        }
    }

    // 挂载
    r.section("3. 挂载（私有 mount ns 内，退出即消失）");
    if !root {
        r.of(St::Warn, "全部跳过", "非 root");
    } else {
        let scratch = "/data/VibeGo/tmp".to_string();
        if std::fs::create_dir_all(&scratch).is_err() {
            r.of(St::Fail, "scratch 目录",
                 &format!("{} 建不了", scratch));
        } else {
            let (_, out) = run_in_child(move || probe_mounts(&scratch));
            r.absorb(&out);
        }
    }

    // rootfs
    r.section("4. rootfs 体检");
    match &o.rootfs {
        None => r.of(St::Info, "未提供 --rootfs", "跳过"),
        Some(rf) => {
            let p = std::path::Path::new(rf);
            let sysd = p.join("usr/lib/systemd/systemd");
            r.of(if p.is_dir() { St::Pass } else { St::Fail },
                 "目录存在", rf);
            r.of(if sysd.exists() { St::Pass } else { St::Fail },
                 "usr/lib/systemd/systemd",
                 if sysd.exists() { "存在" } else { "缺失" });
            let mid = std::fs::read_to_string(p.join("etc/machine-id"))
                .map(|s| s.trim().len() >= 32)
                .unwrap_or(false);
            r.of(if mid { St::Pass } else { St::Warn },
                 "etc/machine-id",
                 if mid { "已存在" } else { "缺失 -> run 会写入" });
            let rc = p.join("etc/resolv.conf").is_file();
            r.of(if rc { St::Pass } else { St::Warn },
                 "etc/resolv.conf",
                 if rc { "已存在" } else { "缺失 -> run 会写入" });
            if p.join("etc/fstab").is_file() {
                r.of(St::Warn, "etc/fstab",
                     "存在 -> 建议清空（systemd 可能照它挂东西）");
            } else {
                r.of(St::Pass, "etc/fstab", "不存在");
            }
            if let Ok(os) = std::fs::read_to_string(p.join("etc/os-release"))
            {
                let l = util::grep_line(&os, "PRETTY_NAME")
                    .unwrap_or_default();
                r.of(St::Info, "发行版", l.trim());
            }
            let w = unsafe {
                libc::access(util::cstr(rf).as_ptr(), libc::W_OK)
            } == 0;
            r.of(if w { St::Pass } else { St::Fail },
                 "目录可写", if w { "yes" } else { "no" });
        }
    }

    r.section("5. 结论");
    println!("  FAIL={}  WARN={}", r.fails, r.warns);
    if r.fails == 0 {
        println!("  -> 可以进入 run --dry-run / run --shell");
    } else {
        println!("  -> 先解决 FAIL 项");
    }
    if r.fails > 0 { 1 } else { 0 }
}

fn probe_mounts(scratch: &str) {
    let c = util::cstr;
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
        util::kv("FAIL", "unshare(NEWNS)", &util::errno_text());
        return;
    }
    let rc = unsafe {
        libc::mount(c("none").as_ptr(), c("/").as_ptr(),
                    std::ptr::null(),
                    libc::MS_REC | libc::MS_PRIVATE, std::ptr::null())
    };
    if rc == 0 {
        util::kv("PASS", "MS_REC|MS_PRIVATE", "OK");
    } else {
        util::kv("WARN", "MS_REC|MS_PRIVATE", &util::errno_text());
    }

    let tmp = format!("{}/tmp", scratch);
    if std::fs::create_dir_all(&tmp).is_err() {
        util::kv("FAIL", "建 scratch/tmp", "失败");
        return;
    }
    let rc = unsafe {
        libc::mount(c("tmpfs").as_ptr(), c(&tmp).as_ptr(),
                    c("tmpfs").as_ptr(), 0,
                    c("mode=755,size=1m").as_ptr() as *const libc::c_void)
    };
    if rc != 0 {
        util::kv("FAIL", "mount tmpfs", &util::errno_text());
        return;
    }
    util::kv("PASS", "mount tmpfs", "OK");

    let cg = format!("{}/cg", tmp);
    let _ = std::fs::create_dir_all(&cg);
    let mut ok = unsafe {
        libc::mount(c("cgroup2").as_ptr(), c(&cg).as_ptr(),
                    c("cgroup2").as_ptr(), 0,
                    c("nsdelegate").as_ptr() as *const libc::c_void)
    };
    if ok == 0 {
        util::kv("PASS", "mount cgroup2", "nsdelegate OK");
    } else {
        let e1 = util::errno_text();
        ok = unsafe {
            libc::mount(c("cgroup2").as_ptr(), c(&cg).as_ptr(),
                        c("cgroup2").as_ptr(), 0, std::ptr::null())
        };
        if ok == 0 {
            util::kv("WARN", "mount cgroup2",
                     &format!("nsdelegate {}，无选项 OK", e1));
        } else {
            util::kv("FAIL", "mount cgroup2", &util::errno_text());
        }
    }
    if ok == 0 {
        let f = format!("{}/cgroup.controllers", cg);
        let ctl = util::read_trim(&f).unwrap_or_default();
        let ctl_s = if ctl.is_empty() {
            "(空：systemd 告警但能启动)".to_string()
        } else { ctl };
        util::kv("INFO", "cgroup2 controllers", &ctl_s);
    }

    let p = format!("{}/proc", tmp);
    let _ = std::fs::create_dir_all(&p);
    let r1 = unsafe {
        libc::mount(c("proc").as_ptr(), c(&p).as_ptr(),
                    c("proc").as_ptr(),
                    libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                    std::ptr::null())
    };
    if r1 == 0 {
        util::kv("PASS", "mount proc", "OK");
    } else {
        util::kv("FAIL", "mount proc", &util::errno_text());
    }

    let dp = format!("{}/pts", tmp);
    let _ = std::fs::create_dir_all(&dp);
    let r2 = unsafe {
        libc::mount(c("devpts").as_ptr(), c(&dp).as_ptr(),
                    c("devpts").as_ptr(),
                    libc::MS_NOSUID | libc::MS_NOEXEC,
                    c("newinstance,ptmxmode=0666,mode=0620,gid=5")
                        .as_ptr() as *const libc::c_void)
    };
    if r2 == 0 {
        util::kv("PASS", "mount devpts", "newinstance OK");
    } else {
        util::kv("WARN", "mount devpts", &util::errno_text());
    }

    for d in [&dp, &p, &cg, &tmp] {
        if unsafe { libc::umount2(c(d).as_ptr(), libc::MNT_DETACH) } == 0 {
            util::kv("PASS", "umount2", d);
        } else {
            util::kv("FAIL", "umount2",
                     &format!("{} {}", d, util::errno_text()));
        }
    }
}
