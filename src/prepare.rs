//! 容器准备：tarball/目录 -> rootfs，以及容器内基础文件。
use std::io::Write;
use std::path::Path;

/// 解包或拷贝 source 到 dest（rootfs）
pub fn unpack(source: &str, dest: &str, log: &mut std::fs::File)
    -> Result<(), String>
{
    let _ = writeln!(log, "unpack: {} -> {}", source, dest);
    let sp = Path::new(source);
    if !sp.exists() {
        return Err(format!("source 不存在: {}", source));
    }
    std::fs::create_dir_all(dest).map_err(|e| format!("{}", e))?;
    if sp.is_dir() {
        return copy_dir(source, dest, log);
    }
    let lower = source.to_lowercase();
    let f = std::fs::File::open(source).map_err(|e| format!("{}", e))?;
    let rdr: Box<dyn std::io::Read> = if lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
    {
        Box::new(flate2::read::GzDecoder::new(f))
    } else if lower.ends_with(".tar.zst") || lower.ends_with(".tzst") {
        let d = ruzstd::StreamingDecoder::new(f)
            .map_err(|e| format!("zstd: {}", e))?;
        Box::new(d)
    } else if lower.ends_with(".tar") {
        Box::new(f)
    } else if lower.ends_with(".tar.xz") || lower.ends_with(".txz") {
        // 纯 Rust 的 lzma-rs 是 push 式，先解成临时 tar 再解包
        return unpack_xz(source, dest, log);
    } else {
        return Err(format!(
            "不支持的格式: {}（支持 .tar/.tar.gz/.tgz/.tar.zst/.tar.xz 或目录）",
            source));
    };
    extract_tar(rdr, dest)?;
    let _ = writeln!(log, "unpack: tar 完成");
    flatten_if_needed(dest, log);
    Ok(())
}

fn unpack_xz(source: &str, dest: &str, log: &mut std::fs::File)
    -> Result<(), String>
{
    let tmp = format!("{}/../.unpack.tmp.tar", dest);
    let _ = writeln!(log, "unpack: xz 先解到临时文件 {}", tmp);
    let fi = std::fs::File::open(source).map_err(|e| format!("{}", e))?;
    let mut fo = std::fs::File::create(&tmp).map_err(|e| format!("{}", e))?;
    let mut br = std::io::BufReader::new(fi);
    lzma_rs::xz_decompress(&mut br, &mut fo)
        .map_err(|e| format!("xz 解压失败: {:?}", e))?;
    drop(fo);
    extract_tar(std::fs::File::open(&tmp)
        .map_err(|e| format!("{}", e))?,
        dest)?;
    let _ = std::fs::remove_file(&tmp);
    let _ = writeln!(log, "unpack: xz 完成");
    flatten_if_needed(dest, log);
    Ok(())
}

fn extract_tar<R: std::io::Read>(r: R, dest: &str) -> Result<(), String> {
    let mut ar = tar::Archive::new(r);
    ar.set_preserve_permissions(true);
    ar.set_overwrite(true);
    ar.set_ignore_zeros(true);
    let entries = ar.entries().map_err(|e| format!("tar entries: {}", e))?;
    for e in entries {
        let mut e = e.map_err(|e| format!("tar 项: {}", e))?;
        let path = e.path().map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "<路径读不了>".to_string());
        e.unpack_in(dest)
            .map_err(|e| format!("解包节 点 {} 失败: {}", path, e))?;
    }
    Ok(())
}

fn copy_dir(src: &str, dest: &str, log: &mut std::fs::File)
    -> Result<(), String>
{
    let _ = writeln!(log, "unpack: 目录拷贝");
    let out = std::process::Command::new("cp")
        .args(["-a", &format!("{}/.", src), dest])
        .status()
        .map_err(|e| format!("cp 起不来: {}", e))?;
    if !out.success() {
        return Err("cp 失败".into());
    }
    let _ = writeln!(log, "unpack: 目录拷贝完成");
    flatten_if_needed(dest, log);
    Ok(())
}

/// 有些 tarball 带一层顶层目录，自动剥掉
fn flatten_if_needed(dest: &str, log: &mut std::fs::File) {
    let has_root = |d: &str| {
        Path::new(&format!("{}/etc", d)).exists()
            || Path::new(&format!("{}/usr", d)).exists()
    };
    if has_root(dest) {
        return;
    }
    let mut subs: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dest) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                subs.push(e.path().to_string_lossy().into_owned());
            }
        }
    }
    if subs.len() == 1 && has_root(&subs[0]) {
        let inner = subs[0].clone();
        let _ = writeln!(log, "unpack: 剥掉顶层目录 {}", inner);
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "cd '{}' && for f in * .[!.]*; do \
                 [ -e \"$f\" ] && mv -f \"$f\" '{}' 2>/dev/null; done",
                inner, dest))
            .status();
        let _ = std::fs::remove_dir(&inner);
    }
}

/// 容器内基础文件：machine-id / resolv.conf / hostname / mask（缺失才写）
pub fn prepare(rootfs: &str, log: &mut std::fs::File) {
    let _ = std::fs::create_dir_all(format!("{}/etc", rootfs));
    let mid = format!("{}/etc/machine-id", rootfs);
    if !has_content(&mid) {
        match std::fs::write(&mid, machine_id()) {
            Ok(_) => { let _ = writeln!(log, "prepare: 写 {}", mid); }
            Err(e) => {
                let _ = writeln!(log, "prepare: {} 写不了: {}", mid, e);
            }
        }
    }
    let rc = format!("{}/etc/resolv.conf", rootfs);
    if !has_content(&rc) {
        let s = "nameserver 8.8.8.8\nnameserver 1.1.1.1\n";
        match std::fs::write(&rc, s) {
            Ok(_) => { let _ = writeln!(log, "prepare: 写 {}", rc); }
            Err(e) => {
                let _ = writeln!(log, "prepare: {} 写不了: {}", rc, e);
            }
        }
    }
    let hn = format!("{}/etc/hostname", rootfs);
    if !has_content(&hn) {
        let _ = std::fs::write(&hn, "vibego\n");
    }
    masks_gpm(rootfs, log);
    masks(rootfs, log);
}

/// gpm.sh 每次登录跑 /usr/bin/tty，在 vibego 的容器里（devpts 是新的
/// 实例，宿主 tty 节点不存在）会刷 “tty: ttyname error: ...”。容器里
/// 没有控制台鼠标，直接停用。
fn masks_gpm(rootfs: &str, log: &mut std::fs::File) {
    let src = format!("{}/etc/profile.d/gpm.sh", rootfs);
    let dst = format!("{}/etc/profile.d/gpm.sh.disabled", rootfs);
    if std::path::Path::new(&src).exists()
        && !std::path::Path::new(&dst).exists()
    {
        match std::fs::rename(&src, &dst) {
            Ok(_) => {
                let _ = writeln!(log, "prepare: 停用 gpm.sh（免 tty 报错）");
            }
            Err(e) => {
                let _ = writeln!(log, "prepare: gpm.sh 停用不了: {}", e);
            }
        }
    }
}

fn has_content(p: &str) -> bool {
    std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false)
}

/// 读 Android 宿主的 DNS。vibego 只在宿主上能跑到这两个工具，
/// 进容器后（prepare 容器内）/ 非 Android 上会安静返回空。
/// 解析 dumpsys connectivity 里的 DnsAddresses（例如
/// DnsAddresses: [ /202.96.134.33,/202.96.128.86,/114.114.114.114 ]）。
pub fn host_dns() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Ok(o) = std::process::Command::new("dumpsys")
        .arg("connectivity").output()
    {
        let text = String::from_utf8_lossy(&o.stdout);
        for line in text.lines() {
            let line = line.trim();
            let Some(pos) = line.find("DnsAddresses:") else {
                continue;
            };
            let after = &line[pos + "DnsAddresses:".len()..];
            let Some(ps) = after.find('[') else { continue };
            let body = &after[ps + 1..];
            let body = body.split(']').next().unwrap_or("");
            for x in body.split(',') {
                let x = x.trim().trim_start_matches('/');
                if x.is_empty() || x.contains(':') || !x.contains('.') {
                    continue;
                }
                if !out.contains(&x.to_string()) {
                    out.push(x.to_string());
                }
            }
            if !out.is_empty() {
                break;
            }
        }
    }
    out
}

/// 确定要写进容器的 DNS：--dns 指定优先，其次宿主 DNS，最后缺省。
pub fn pick_dns(custom: Option<&str>) -> Vec<String> {
    if let Some(c) = custom {
        let v: Vec<String> = c.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect();
        if !v.is_empty() {
            return v;
        }
    }
    let h = host_dns();
    if !h.is_empty() {
        return h;
    }
    vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()]
}

/// 写入容器 resolv.conf（创建时整文件覆盖写）。servers 空则不动。
pub fn write_dns(path: &str, servers: &[String], log: &mut std::fs::File)
{
    if servers.is_empty() {
        return;
    }
    let mut s = String::new();
    for ns in servers {
        s.push_str(&format!("nameserver {}\n", ns));
    }
    s.push_str("options timeout:2 attempts:2\n");
    match std::fs::write(path, s) {
        Ok(_) => {
            let _ = writeln!(log, "prepare: 写 DNS {} → {}", 
                             servers.join(","), path);
        }
        Err(e) => {
            let _ = writeln!(log, "prepare: 写 {} 失败: {}", path, e);
        }
    }
}

pub fn machine_id() -> String {
    use std::io::Read;
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

pub fn masks(rootfs: &str, log: &mut std::fs::File) {
    let dir = format!("{}/etc/systemd/system", rootfs);
    let _ = std::fs::create_dir_all(&dir);
    let units = [
        "systemd-networkd.service",
        "systemd-networkd.socket",
        "systemd-networkd-wait-online.service",
        "systemd-resolved.service",
        "systemd-udevd.service",
        "systemd-udevd-control.socket",
        "systemd-udevd-kernel.socket",
        "systemd-remount-fs.service",
        "systemd-tmpfiles-setup-dev.service",
        "systemd-tmpfiles-setup-dev-early.service",
    ];
    for u in units {
        let p = format!("{}/{}", dir, u);
        if std::path::Path::new(&p).exists() {
            continue;
        }
        if std::os::unix::fs::symlink("/dev/null", &p).is_ok() {
            let _ = writeln!(log, "prepare: mask {}", u);
        }
    }
}
