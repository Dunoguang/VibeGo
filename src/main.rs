//! vibego CLI
mod boot;
mod config;
mod enter;
mod manage;
mod new;
mod prepare;
mod probe;
mod start;
mod util;

use std::process::ExitCode;

const USAGE: &str = "\
vibego 0.1.0 - Android 上的 PID1 容器运行器（systemd as PID 1）

用法:
  vibego new   --name NAME --source FILE [--path DIR] [--host-data] [--dns 8.8.8.8,114.114.114.114]
  vibego start NAME [-f|--foreground] [--shell]
  vibego enter NAME [-- CMD...]
  vibego stop  NAME [--timeout SEC] [--graceful]
  vibego list
  vibego logs  NAME [-f|--follow] [--lines N]
  vibego rm    NAME [--force]
  vibego enable NAME / disable NAME   # 开机自启开关
  vibego autostart                     # 启动所有开了自启的容器
  vibego probe [--rootfs DIR]

全局: --base DIR（默认 /data/VibeGo）

说明:
  start 默认后台（日志落 <path>/log/vibego.log）
  enter 默认 bash -il（无 bash 用 sh），也可 vibego enter X -- ls /
  net/user namespace 不动，容器直接用手机的网络
  new 不传 --dns 时自动写宿主 DNS（dumpsys 探测），失败才用缺省
  new 默认开启开机自启（KSU service.d），--no-autostart 可关
";

/// probe 子命令用
pub struct Opts {
    pub rootfs: Option<String>,
    pub scratch: String,
}

fn arg_val(rest: &[String], key: &str) -> Option<String> {
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == key {
            return rest.get(i + 1).cloned();
        }
        i += 1;
    }
    None
}

fn has_flag(rest: &[String], key: &str) -> bool {
    rest.iter().any(|x| x == key)
}

const WITH_VAL: [&str; 7] =
    ["--name", "--path", "--source", "--timeout", "--lines", "--base",
     "--dns"];

fn first_pos(rest: &[String]) -> Option<String> {
    let mut i = 0;
    while i < rest.len() {
        let a = &rest[i];
        if a == "--" {
            return rest.get(i + 1).cloned();
        }
        if a.starts_with("--") {
            i += if WITH_VAL.contains(&a.as_str()) { 2 } else { 1 };
            continue;
        }
        if a.starts_with('-') && a.len() > 1 {
            i += 1;
            continue;
        }
        return Some(a.clone());
    }
    None
}

fn after_dashdash(rest: &[String]) -> Vec<String> {
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "--" {
            return rest[i + 1..].to_vec();
        }
        i += 1;
    }
    Vec::new()
}

fn resolve_name(base: &str, args: &[String])
    -> Result<config::Container, String>
{
    let n = arg_val(args, "--name")
        .or_else(|| first_pos(args))
        .ok_or_else(|| "需要容器名".to_string())?;
    config::resolve(base, &n)
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let mut base = config::DEFAULT_BASE.to_string();
    let mut rest: Vec<String> = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        if argv[i] == "--base" {
            i += 1;
            if let Some(v) = argv.get(i) {
                base = v.clone();
            }
        } else {
            rest.push(argv[i].clone());
        }
        i += 1;
    }
    let sub = rest.first().cloned().unwrap_or_default();
    let args: Vec<String> = if rest.len() > 1 {
        rest[1..].to_vec()
    } else {
        Vec::new()
    };

    let code: i32 = match sub.as_str() {
        "new" => {
            let name = arg_val(&args, "--name")
                .or_else(|| first_pos(&args));
            let source = arg_val(&args, "--source").unwrap_or_default();
            match name {
                Some(n) => new::new(
                    &base,
                    &n,
                    arg_val(&args, "--path").as_deref(),
                    &source,
                    !has_flag(&args, "--no-host-data"),
                    arg_val(&args, "--dns").as_deref(),
                    !has_flag(&args, "--no-autostart"),
                ),
                None => {
                    eprintln!("new 需要 --name 和 --source\n{}", USAGE);
                    2
                }
            }
        }
        "start" => match resolve_name(&base, &args) {
            Ok(c) => {
                let fg = has_flag(&args, "--foreground")
                    || has_flag(&args, "-f");
                start::start(&c, !fg, has_flag(&args, "--shell"))
            }
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        },
        "enter" => match resolve_name(&base, &args) {
            Ok(c) => enter::enter(&c, &after_dashdash(&args)),
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        },
        "stop" => match resolve_name(&base, &args) {
            Ok(c) => {
                let t = arg_val(&args, "--timeout")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(15);
                manage::stop(&c, t, has_flag(&args, "--graceful"))
            }
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        },
        "list" | "ls" | "ps" => manage::list(&base),
        "autostart" => manage::autostart(&base),
        "enable" => match resolve_name(&base, &args) {
            Ok(c) => manage::set_autostart(&c, true),
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        },
        "disable" => match resolve_name(&base, &args) {
            Ok(c) => manage::set_autostart(&c, false),
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        },
        "logs" => match resolve_name(&base, &args) {
            Ok(c) => {
                let n = arg_val(&args, "--lines")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(50);
                let f = has_flag(&args, "--follow") || has_flag(&args, "-f");
                manage::logs(&c, f, n)
            }
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        },
        "rm" => match resolve_name(&base, &args) {
            Ok(c) => manage::rm(&base, &c, has_flag(&args, "--force")),
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        },
        "probe" => {
            let o = Opts {
                rootfs: arg_val(&args, "--rootfs"),
                scratch: format!("{}/tmp", base),
            };
            probe::run(&o)
        }
        "" | "-h" | "--help" | "help" => {
            print!("{}", USAGE);
            0
        }
        other => {
            eprintln!("未知子命令: {}\n{}", other, USAGE);
            2
        }
    };
    ExitCode::from(code as u8)
}
