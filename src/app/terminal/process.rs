use std::path::PathBuf;

/// The working directory of process `pid`, where the platform can report it.
#[cfg(target_os = "macos")]
pub(super) fn working_directory(pid: u32) -> Option<PathBuf> {
    use std::{ffi::CStr, os::unix::ffi::OsStrExt};

    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: the buffer is a zeroed `proc_vnodepathinfo` of exactly `size`
    // bytes, which is what this flavor writes.
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: the call filled the whole structure.
    let info = unsafe { info.assume_init() };
    let path = &info.pvi_cdir.vip_path;
    // SAFETY: `vip_path` is a contiguous `MAXPATHLEN` character array that the
    // kernel NUL-terminates.
    let bytes = unsafe {
        std::slice::from_raw_parts(path.as_ptr().cast::<u8>(), std::mem::size_of_val(path))
    };
    let path = CStr::from_bytes_until_nul(bytes).ok()?.to_bytes();
    (!path.is_empty()).then(|| std::ffi::OsStr::from_bytes(path).into())
}

#[cfg(target_os = "linux")]
pub(super) fn working_directory(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn working_directory(_pid: u32) -> Option<PathBuf> {
    None
}
