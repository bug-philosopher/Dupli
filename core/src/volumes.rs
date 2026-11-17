use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeKind {
    Internal,
    External,
    Network,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeInfo {
    pub mount: String,
    pub name: String,
    pub kind: VolumeKind,
    pub fs: String,
    pub server: Option<String>,
    pub serial: Option<String>,
    pub total: u64,
    pub free: u64,
}

// Find the drive that contains the path.
pub fn volume_info(path: &Path) -> Option<VolumeInfo> {
    imp::volume_info(path)
}

pub fn free_space(path: &Path) -> Option<u64> {
    volume_info(path).map(|v| v.free)
}

pub fn is_fat(fs: &str) -> bool {
    matches!(fs.to_ascii_lowercase().as_str(), "msdos" | "vfat" | "fat" | "fat12" | "fat16" | "fat32")
}

pub fn eject(info: &VolumeInfo) -> Result<(), String> {
    if info.kind == VolumeKind::Internal {
        return Err("Built-in drives cannot be ejected.".into());
    }
    imp::eject(info)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    match std::process::Command::new(program).args(args).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let text = String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr });
            Err(text.trim().to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    use std::sync::OnceLock;

    pub fn volume_info(path: &Path) -> Option<VolumeInfo> {
        let c = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut s: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
            return None;
        }
        let text = |raw: &[libc::c_char]| unsafe { CStr::from_ptr(raw.as_ptr()) }.to_string_lossy().into_owned();
        let mount = text(&s.f_mntonname);
        let from = text(&s.f_mntfromname);
        let fs = text(&s.f_fstypename);
        let local = s.f_flags & (libc::MNT_LOCAL as u32) != 0;
        let block = s.f_bsize as u64;

        let kind = if !local {
            VolumeKind::Network
        } else if mount.starts_with("/Volumes/") {
            VolumeKind::External
        } else {
            VolumeKind::Internal
        };
        let name = match kind {
            VolumeKind::Internal => startup_name().to_owned(),
            _ => Path::new(&mount).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| mount.clone()),
        };
        Some(VolumeInfo {
            server: (kind == VolumeKind::Network).then(|| server_of(&from)).flatten(),
            mount,
            name,
            kind,
            fs,
            serial: None,
            total: s.f_blocks as u64 * block,
            free: s.f_bavail as u64 * block,
        })
    }

    fn server_of(from: &str) -> Option<String> {
        let host = if let Some(rest) = from.strip_prefix("//") {
            let host = rest.split('/').next()?;
            host.rsplit('@').next()?
        } else {
            from.split(':').next()?
        };
        (!host.is_empty()).then(|| host.to_owned())
    }

    fn startup_name() -> &'static str {
        static NAME: OnceLock<String> = OnceLock::new();
        NAME.get_or_init(|| {
            std::fs::read_dir("/Volumes")
                .ok()
                .and_then(|list| {
                    list.flatten().find_map(|e| {
                        let target = std::fs::read_link(e.path()).ok()?;
                        (target == Path::new("/")).then(|| e.file_name().to_string_lossy().into_owned())
                    })
                })
                .unwrap_or_else(|| "This Mac".to_owned())
        })
    }

    pub fn eject(info: &VolumeInfo) -> Result<(), String> {
        let verb = if info.kind == VolumeKind::Network { "unmount" } else { "eject" };
        run("/usr/sbin/diskutil", &[verb, &info.mount])
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const NETWORK: &[&str] = &["cifs", "smb3", "smbfs", "nfs", "nfs4", "afpfs", "davfs", "9p", "ceph", "glusterfs"];

    pub fn volume_info(path: &Path) -> Option<VolumeInfo> {
        let real = std::fs::canonicalize(path).ok()?;
        let table = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
        let mut best: Option<(String, String, String)> = None;
        for line in table.lines() {
            let Some((left, right)) = line.split_once(" - ") else { continue };
            let Some(point) = left.split(' ').nth(4).map(unescape) else { continue };
            let mut tail = right.split(' ');
            let (Some(fs), Some(source)) = (tail.next(), tail.next()) else { continue };
            if real.starts_with(&point) && best.as_ref().is_none_or(|b| point.len() >= b.0.len()) {
                best = Some((point, fs.to_owned(), unescape(source)));
            }
        }
        let (mount, fs, source) = best?;

        let c = CString::new(real.as_os_str().as_bytes()).ok()?;
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
            return None;
        }
        let block = s.f_frsize as u64;

        let network = NETWORK.contains(&fs.as_str()) || fs.starts_with("fuse.ssh") || fs.starts_with("fuse.rclone");
        let kind = if network {
            VolumeKind::Network
        } else if ["/media/", "/run/media/", "/mnt/"].iter().any(|p| mount.starts_with(p)) {
            VolumeKind::External
        } else {
            VolumeKind::Internal
        };
        let name = match kind {
            VolumeKind::Internal => "This computer".to_owned(),
            _ => Path::new(&mount).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| mount.clone()),
        };
        let server = network
            .then(|| {
                let s = source.trim_start_matches('/');
                s.split(['/', ':']).next().map(|h| h.rsplit('@').next().unwrap_or(h).to_owned())
            })
            .flatten()
            .filter(|h| !h.is_empty());
        Some(VolumeInfo {
            mount,
            name,
            kind,
            fs,
            server,
            serial: None,
            total: s.f_blocks as u64 * block,
            free: s.f_bavail as u64 * block,
        })
    }

    fn unescape(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 3 < bytes.len() && bytes[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b)) {
                let v = (bytes[i + 1] - b'0') as u32 * 64 + (bytes[i + 2] - b'0') as u32 * 8 + (bytes[i + 3] - b'0') as u32;
                out.push(v as u8);
                i += 4;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    pub fn eject(info: &VolumeInfo) -> Result<(), String> {
        run("gio", &["mount", "-u", &info.mount]).or_else(|_| run("umount", &[&info.mount]))
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW,
    };

    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_REMOTE: u32 = 4;

    fn text(buf: &[u16]) -> String {
        let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    pub fn volume_info(path: &Path) -> Option<VolumeInfo> {
        if !path.exists() {
            return None;
        }
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut root = [0u16; 520];
        if unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
            return None;
        }
        let mount = text(&root);
        let drive = unsafe { GetDriveTypeW(root.as_ptr()) };

        let mut label = [0u16; 262];
        let mut fs = [0u16; 262];
        let (mut serial, mut max_len, mut flags) = (0u32, 0u32, 0u32);
        let known = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                &mut serial,
                &mut max_len,
                &mut flags,
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        } != 0;
        let (mut free, mut total, mut all_free) = (0u64, 0u64, 0u64);
        if unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut free, &mut total, &mut all_free) } == 0 {
            return None;
        }

        let unc = mount.strip_prefix(r"\\?\UNC\").or_else(|| mount.strip_prefix(r"\\").filter(|m| !m.starts_with('?')));
        let mut parts = unc.unwrap_or("").split('\\').filter(|p| !p.is_empty());
        let (server, share) = (parts.next().map(str::to_owned), parts.next().map(str::to_owned));

        let kind = if drive == DRIVE_REMOTE || unc.is_some() {
            VolumeKind::Network
        } else if drive == DRIVE_REMOVABLE {
            VolumeKind::External
        } else {
            VolumeKind::Internal
        };
        let label = text(&label);
        let letter = mount.trim_start_matches(r"\\?\").trim_end_matches('\\').to_owned();
        let name = match (&share, label.is_empty()) {
            (Some(share), _) => share.clone(),
            (None, false) => format!("{label} ({letter})"),
            (None, true) => letter,
        };
        Some(VolumeInfo {
            mount,
            name,
            kind,
            fs: text(&fs),
            server,
            serial: (known && kind != VolumeKind::Network).then(|| format!("{serial:08X}")),
            total,
            free,
        })
    }

    pub fn eject(_info: &VolumeInfo) -> Result<(), String> {
        Err("Use \u{201c}Safely Remove Hardware\u{201d} in the Windows taskbar to eject this drive.".into())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod imp {
    use super::*;

    pub fn volume_info(_path: &Path) -> Option<VolumeInfo> {
        None
    }

    pub fn eject(_info: &VolumeInfo) -> Result<(), String> {
        Err("Ejecting is not supported on this system.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_temp_folder_is_on_a_volume_with_room() {
        let dir = tempfile::tempdir().unwrap();
        let v = volume_info(dir.path()).expect("volume");
        assert!(v.total > 0 && v.free <= v.total, "{v:?}");
        assert!(!v.mount.is_empty() && !v.name.is_empty());
        assert_eq!(volume_info(&dir.path().join("missing")), None);
    }

    #[test]
    fn only_real_fat_shifts_with_daylight_saving() {
        assert!(is_fat("msdos") && is_fat("FAT32") && is_fat("vfat"));
        assert!(!is_fat("exfat") && !is_fat("apfs") && !is_fat("NTFS"));
    }
}
