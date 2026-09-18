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
    let mut ar = tar::Archive::new(rdr);
    ar.set_preserve_permissions(true);
    ar.set_overwrite(true);
    ar.unpack(dest).map_err(|e| format!("tar 解包失败: {}", e))?;
    let _ = writeln!(log, "unpack: tar 完成");
    flatten_if_needed(dest, log);
    Ok(())
}

fn unpack_xz(source: &str, dest: &str, log: &mut std::fs::File)
    -> Result<(), String>
{
    let tmp = format!("{}/../.unpack.tmp.tar", dest);
    let _ = writeln!(log, "unpack: xz 先解到临时文件 {}", tmp);
    let mut fi = std::fs::File::open(source).map_err(|e| format!("{}", e))?;
    let mut fo = std::fs::File::create(&tmp).map_err(|e| format!("{}", e))?;
    lzma_rs::xz_decompress(&mut fi, &mut fo)
        .map_err(|e| format!("xz 解压失败: {:?}", e))?;
    drop(fo);
    let r = {
        let f = std::fs::File::open(&tmp).map_err(|e| format!("{}", e))?;
        let mut ar = tar::Archive::new(f);
        ar.set_preserve_permissions(true);
        ar.unpack(dest).map_err(|e| format!("tar 解包失败: {}", e))
    };
    let _ = std::fs::remove_file(&tmp);
    r?;
    let _ = writeln!(log, "unpack: xz 完成");
    flatten_if_needed(dest, log);
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
    masks(rootfs, log);
}

fn has_content(p: &str) -> bool {
    std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false)
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
