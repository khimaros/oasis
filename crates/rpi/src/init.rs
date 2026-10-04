//! what the first process of a linux system has to do before it can use
//! devices: mount the kernel's file systems and load driver modules.

use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

const MODULES_DIR: &str = "/lib/modules";
const RELEASE_FILE: &str = "/proc/sys/kernel/osrelease";
const DEPENDENCIES_FILE: &str = "modules.dep";
const MODULE_SUFFIX: &str = ".ko";
/// where the kernel shows devices, processes, and drivers
const SYSTEM_MOUNTS: [(&str, &str); 3] = [("devtmpfs", "/dev"), ("proc", "/proc"), ("sysfs", "/sys")];
const WAIT_STEP: Duration = Duration::from_millis(50);
const BLOCK_DIR: &str = "/sys/class/block";
const DEVICE_DIR: &str = "/dev";
// where an ext4 file system keeps its magic number and its label
const SUPERBLOCK_AT: u64 = 1024;
const MAGIC_AT: usize = 56;
const EXT_MAGIC: [u8; 2] = [0x53, 0xef];
const LABEL_AT: usize = 120;
const LABEL_BYTES: usize = 16;

fn checked(result: i64) -> io::Result<()> {
    if result == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

fn c_string(text: &str) -> io::Result<CString> {
    CString::new(text).map_err(io::Error::other)
}

/// mounts the file system of type `kind` on `source` at `target`.
pub fn mount(source: &str, target: &str, kind: &str, flags: libc::c_ulong) -> io::Result<()> {
    fs::create_dir_all(target)?;
    let (source, target, kind) = (c_string(source)?, c_string(target)?, c_string(kind)?);
    let result = unsafe { libc::mount(source.as_ptr(), target.as_ptr(), kind.as_ptr(), flags, ptr::null()) };
    checked(result.into())
}

pub fn mount_system() -> io::Result<()> {
    SYSTEM_MOUNTS.iter().try_for_each(|(kind, target)| mount(kind, target, kind, 0))
}

/// loads one module file. fine when it is loaded already.
fn insert(path: &Path) -> io::Result<()> {
    let file = File::open(path)?;
    let result = unsafe { libc::syscall(libc::SYS_finit_module, file.as_raw_fd(), c"".as_ptr(), 0) };
    match checked(result) {
        Err(err) if err.raw_os_error() == Some(libc::EEXIST) => Ok(()),
        result => result,
    }
}

/// loads the module `name` after the modules it depends on, like the
/// modprobe command. the kernel names modules with `-` and `_` alike.
pub fn modprobe(name: &str) -> io::Result<()> {
    let dir = Path::new(MODULES_DIR).join(fs::read_to_string(RELEASE_FILE)?.trim());
    let dependencies = fs::read_to_string(dir.join(DEPENDENCIES_FILE))?;
    let wanted = format!("{}{MODULE_SUFFIX}", name.replace('-', "_"));
    let is_wanted = |path: &str| path.rsplit('/').next().is_some_and(|file| file.replace('-', "_") == wanted);
    // a line names a module file, then the files it needs, nearest first
    let files = dependencies.lines().map(|line| line.split([':', ' ']).filter(|file| !file.is_empty()));
    let mut found = files.map(Vec::from_iter).find(|files| files.first().is_some_and(|file| is_wanted(file)));
    let missing = || io::Error::new(io::ErrorKind::NotFound, format!("no module {name}"));
    found.take().ok_or_else(missing)?.iter().rev().try_for_each(|file| insert(&dir.join(file)))
}

fn has_label(device: &Path, label: &str) -> bool {
    let mut superblock = [0u8; LABEL_AT + LABEL_BYTES];
    let read = File::open(device).and_then(|mut file| {
        file.seek(SeekFrom::Start(SUPERBLOCK_AT))?;
        file.read_exact(&mut superblock)
    });
    let stored = superblock[LABEL_AT..].split(|byte| *byte == 0).next();
    read.is_ok()
        && superblock[MAGIC_AT..MAGIC_AT + EXT_MAGIC.len()] == EXT_MAGIC
        && stored == Some(label.as_bytes())
}

/// the block device that holds the ext4 file system labeled `label`. the
/// kernel numbers disks by the order it finds them in, the label stays.
pub fn find_filesystem(label: &str) -> Option<PathBuf> {
    let names = fs::read_dir(BLOCK_DIR).ok()?.flatten().map(|entry| entry.file_name());
    names.map(|name| Path::new(DEVICE_DIR).join(name)).find(|device| has_label(device, label))
}

/// waits until `ready` holds. devices show up a while after their driver
/// was loaded.
pub fn wait_for(what: &str, timeout: Duration, ready: impl Fn() -> bool) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    while !ready() {
        if Instant::now() > deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("{what} did not show up")));
        }
        thread::sleep(WAIT_STEP);
    }
    Ok(())
}

/// (used, total) bytes of the file system that holds `path`.
pub fn filesystem(path: &str) -> io::Result<(u64, u64)> {
    let path = c_string(path)?;
    let mut stats = unsafe { std::mem::zeroed::<libc::statvfs>() };
    checked(unsafe { libc::statvfs(path.as_ptr(), &mut stats) }.into())?;
    let block = stats.f_frsize as u64;
    let (total, free) = (stats.f_blocks as u64 * block, stats.f_bfree as u64 * block);
    Ok((total - free, total))
}

/// writes out what is cached and restarts the machine.
pub fn reboot() {
    unsafe {
        libc::sync();
        libc::reboot(libc::RB_AUTOBOOT);
    }
}
