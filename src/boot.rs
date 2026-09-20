//! 开机自启：安装 KernelSU service.d 脚本。
use std::io::Write;

const DIR: &str = "/data/adb/service.d";
const NAME: &str = "vibego.sh";

/// script 路径
pub fn path() -> String {
    format!("{}/{}", DIR, NAME)
}

/// 纯逻辑：写脚本 + chmod 755。返回 Err 说明没装成
pub fn install_to_disk() -> Result<String, String> {
    if !std::path::Path::new(DIR).is_dir() {
        return Err(format!("{} 不存在（不是 KernelSU？）", DIR));
    }
    let p = path();
    let body = script();
    std::fs::write(&p, body).map_err(|e| format!("{}: {}", p, e))?;
    use std::os::unix::fs::PermissionsExt;
    let m = std::fs::Permissions::from_mode(0o755);
    let _ = std::fs::set_permissions(&p, m);
    Ok(p)
}

/// new 时调用：装脚本，结果写进日志
pub fn install(log: &mut std::fs::File) {
    match install_to_disk() {
        Ok(p) => {
            let _ = writeln!(log, "boot: 安装 {}", p);
        }
        Err(e) => {
            let _ = writeln!(log, "boot: 跳过自启安装 ({})", e);
        }
    }
}

/// service.d 脚本内容：等 sys.boot_completed 再 vibego autostart
fn script() -> String {
    let mut s = String::new();
    s.push_str("#!/system/bin/sh\n");
    s.push_str("# vibego 开机自启（由 vibego new 安装）\n");
    s.push_str("VG=/data/VibeGo/vibego\n");
    s.push_str("LOG=/data/VibeGo/autostart.log\n");
    s.push_str("[ -x \"$VG\" ] || exit 0\n");
    s.push_str("echo \"[$(date)] service.d 触发\" >> $LOG\n");
    s.push_str("i=0\n");
    s.push_str("while [ $i -lt 90 ]; do\n");
    s.push_str("  if [ \"$(getprop sys.boot_completed)\" = 1 ]; then\n");
    s.push_str("    break\n");
    s.push_str("  fi\n");
    s.push_str("  sleep 2\n");
    s.push_str("  i=$((i+1))\n");
    s.push_str("done\n");
    s.push_str("sleep 5\n");
    s.push_str("\"$VG\" autostart >> $LOG 2>&1\n");
    s
}
