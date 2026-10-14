//! Per-OS process primitives shared by the Reap and the Settle (#29 D3, D8; #37 D5).
//!
//! Each call is a synchronous OS query or signal, and nothing here waits. Some are **blocking**:
//! on Linux the stamp, the group members and the env marker read `/proc` with `std::fs`, so async
//! code calls [`process_stamp`], [`group_members`] and [`has_env_entry`] through
//! `spawn_blocking` (#27, architecture §2). `kill(2)` probes and signals are fine inline.

use std::io;

use super::ProcessStamp;

/// The OS start stamp of a live `pid` (#29 D2). A pid that doesn't exist is
/// [`io::ErrorKind::NotFound`].
#[cfg(target_os = "macos")]
pub(crate) fn process_stamp(pid: u32) -> io::Result<ProcessStamp> {
    let pid = to_c_pid(pid)?;
    // SAFETY: an all-zero `proc_bsdinfo` is a valid value of a plain C struct.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is writable and `size` is its exact size.
    let n =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    if n != size {
        let err = io::Error::last_os_error();
        return Err(if n <= 0 && err.raw_os_error() == Some(libc::ESRCH) {
            io::ErrorKind::NotFound.into()
        } else if n <= 0 {
            err
        } else {
            io::Error::other("short proc_pidinfo")
        });
    }
    Ok(ProcessStamp::Macos {
        sec: info.pbi_start_tvsec,
        usec: info.pbi_start_tvusec,
    })
}

/// The OS start stamp of a live `pid` (#29 D2). A pid that doesn't exist is
/// [`io::ErrorKind::NotFound`].
#[cfg(target_os = "linux")]
pub(crate) fn process_stamp(pid: u32) -> io::Result<ProcessStamp> {
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let start_ticks = parse_stat(&stat)
        .map(|s| s.start_ticks)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unparsable /proc stat"))?;
    Ok(ProcessStamp::Linux {
        boot_id: boot_id.trim().to_owned(),
        start_ticks,
    })
}

/// The OS start stamp of a live `pid` (#29 D2). A pid that doesn't exist, or that has exited, is
/// [`io::ErrorKind::NotFound`].
#[cfg(windows)]
pub(crate) fn process_stamp(pid: u32) -> io::Result<ProcessStamp> {
    let p = win::Process::open_query(pid)?;
    if !p.is_running() {
        return Err(io::ErrorKind::NotFound.into());
    }
    Ok(ProcessStamp::Windows {
        creation_time: p.creation_time()?,
    })
}

/// No stamp source on this OS: every entry is unprovable.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
pub(crate) fn process_stamp(_pid: u32) -> io::Result<ProcessStamp> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(unix)]
fn to_c_pid(pid: u32) -> io::Result<libc::pid_t> {
    libc::pid_t::try_from(pid)
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))
}

#[cfg(unix)]
fn nix_pid(pid: u32) -> io::Result<nix::unistd::Pid> {
    to_c_pid(pid).map(nix::unistd::Pid::from_raw)
}

/// Whether `pid` names a process, zombies included (`kill(pid, 0)`; `EPERM` means it exists).
#[cfg(unix)]
pub(crate) fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = nix_pid(pid) else { return false };
    !matches!(
        nix::sys::signal::kill(pid, None),
        Err(nix::errno::Errno::ESRCH)
    )
}

/// Whether process group `pgid` has any member (`kill(-pgid, 0)`; `ESRCH` means empty, #37 D2).
#[cfg(unix)]
pub(crate) fn group_alive(pgid: u32) -> bool {
    let Ok(pgid) = nix_pid(pgid) else {
        return false;
    };
    !matches!(
        nix::sys::signal::killpg(pgid, None),
        Err(nix::errno::Errno::ESRCH)
    )
}

/// Sends `signal` to every member of group `pgid`. An empty group is not an error.
#[cfg(unix)]
pub(crate) fn signal_group(pgid: u32, signal: nix::sys::signal::Signal) -> io::Result<()> {
    match nix::sys::signal::killpg(nix_pid(pgid)?, signal) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// The members of process group `pgid` (#37 D5's count; #29 D3's marker search).
#[cfg(target_os = "macos")]
pub(crate) fn group_members(pgid: u32) -> Vec<u32> {
    /// `PROC_PGRP_ONLY` from `<sys/proc_info.h>`; `libc` doesn't export it.
    const PROC_PGRP_ONLY: u32 = 2;
    // SAFETY: a null buffer asks for the size only.
    let bytes = unsafe { libc::proc_listpids(PROC_PGRP_ONLY, pgid, std::ptr::null_mut(), 0) };
    if bytes <= 0 {
        return Vec::new();
    }
    let mut pids: Vec<libc::c_int> = vec![0; bytes as usize / size_of::<libc::c_int>() + 16];
    let cap = (pids.len() * size_of::<libc::c_int>()) as libc::c_int;
    // SAFETY: `pids` is writable for `cap` bytes.
    let bytes = unsafe { libc::proc_listpids(PROC_PGRP_ONLY, pgid, pids.as_mut_ptr().cast(), cap) };
    if bytes <= 0 {
        return Vec::new();
    }
    pids.truncate(bytes as usize / size_of::<libc::c_int>());
    pids.into_iter()
        .filter_map(|p| u32::try_from(p).ok())
        .filter(|p| *p > 0)
        .collect()
}

/// The members of process group `pgid` (#37 D5's count; #29 D3's marker search).
#[cfg(target_os = "linux")]
pub(crate) fn group_members(pgid: u32) -> Vec<u32> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|s| parse_stat(&s))
                .is_some_and(|s| s.pgrp == pgid)
        })
        .collect()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
pub(crate) fn group_members(_pgid: u32) -> Vec<u32> {
    Vec::new()
}

/// Whether `pid`'s environment holds exactly `entry` (`KEY=value`), #29 D3's fallback. Linux
/// reads `/proc/<pid>/environ`.
#[cfg(target_os = "linux")]
pub(crate) fn has_env_entry(pid: u32, entry: &[u8]) -> bool {
    std::fs::read(format!("/proc/{pid}/environ"))
        .is_ok_and(|env| env.split(|b| *b == 0).any(|e| e == entry))
}

/// Whether `pid`'s environment holds exactly `entry` (`KEY=value`), #29 D3's fallback. macOS
/// reads `KERN_PROCARGS2`, which is empty for platform binaries (#29 Evidence), so this is best
/// effort.
#[cfg(target_os = "macos")]
pub(crate) fn has_env_entry(pid: u32, entry: &[u8]) -> bool {
    let Ok(pid) = to_c_pid(pid) else { return false };
    let mut argmax: libc::c_int = 0;
    let mut size = size_of::<libc::c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    // SAFETY: `argmax` is writable for `size` bytes.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&raw mut argmax).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || argmax <= 0 {
        return false;
    }
    let mut buf = vec![0u8; argmax as usize];
    let mut size = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: `buf` is writable for `size` bytes.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return false;
    }
    buf.truncate(size);
    procargs2_env(&buf).any(|e| e == entry)
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
pub(crate) fn has_env_entry(_pid: u32, _entry: &[u8]) -> bool {
    false
}

/// The environment strings of a `KERN_PROCARGS2` buffer: `argc`, the exec path, NUL padding,
/// `argc` arguments, then the environment up to an empty string.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn procargs2_env(buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    let argc = buf
        .get(..4)
        .map_or(0, |b| i32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
        .max(0) as usize;
    let rest = buf.get(4..).unwrap_or_default();
    // The exec path, then NUL padding.
    let after_path = rest.iter().position(|b| *b == 0).map_or(rest.len(), |i| i);
    let rest = &rest[after_path..];
    let start = rest.iter().position(|b| *b != 0).unwrap_or(rest.len());
    let mut strings = rest[start..].split(|b| *b == 0);
    for _ in 0..argc {
        strings.next();
    }
    strings.take_while(|s| !s.is_empty())
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Stat {
    pub(crate) pgrp: u32,
    pub(crate) start_ticks: u64,
}

/// Parses `/proc/<pid>/stat`: field 5 is `pgrp`, field 22 `starttime`. `comm` (field 2) may hold
/// spaces and parentheses, so fields are counted from the last `)`.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_stat(stat: &str) -> Option<Stat> {
    let after = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = after.split_whitespace().collect();
    // `after` starts at field 3 (`state`).
    Some(Stat {
        pgrp: fields.get(5 - 3)?.parse().ok()?,
        start_ticks: fields.get(22 - 3)?.parse().ok()?,
    })
}

#[cfg(windows)]
pub(crate) mod win {
    //! Windows process handles, creation times and the run's accounting Job object (#29 D3,
    //! #37 D2 step 3).

    use std::io;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA, FILETIME, HANDLE, STILL_ACTIVE,
        WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOBOBJECT_BASIC_PROCESS_ID_LIST,
        JobObjectBasicProcessIdList, QueryInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
    };

    /// An owned process handle.
    pub(crate) struct Process(HANDLE);

    // SAFETY: a process handle may be used from any thread.
    unsafe impl Send for Process {}

    impl Process {
        /// Opens `pid` for queries. A pid that doesn't exist is `NotFound`.
        pub(crate) fn open_query(pid: u32) -> io::Result<Self> {
            Self::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)
        }

        /// Opens `pid` for queries, termination and waiting, so the process that is verified is
        /// the one that is killed: the handle pins it and its pid can't be reused (#29 D3).
        pub(crate) fn open_for_kill(pid: u32) -> io::Result<Self> {
            Self::open(
                pid,
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
            )
        }

        fn open(pid: u32, access: u32) -> io::Result<Self> {
            // SAFETY: plain FFI call; a null result is checked.
            let h = unsafe { OpenProcess(access, 0, pid) };
            if h.is_null() {
                let err = io::Error::last_os_error();
                return Err(
                    if err.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                        io::ErrorKind::NotFound.into()
                    } else {
                        err
                    },
                );
            }
            Ok(Self(h))
        }

        pub(crate) fn is_running(&self) -> bool {
            let mut code = 0u32;
            // SAFETY: `self.0` is a valid handle and `code` is writable.
            let ok = unsafe { GetExitCodeProcess(self.0, &mut code) };
            ok != 0 && code == STILL_ACTIVE as u32
        }

        /// `GetProcessTimes`' creation time as a FILETIME value.
        pub(crate) fn creation_time(&self) -> io::Result<u64> {
            let zero = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut c, mut e, mut k, mut u) = (zero, zero, zero, zero);
            // SAFETY: `self.0` is a valid handle; the four FILETIMEs are writable.
            let ok = unsafe { GetProcessTimes(self.0, &mut c, &mut e, &mut k, &mut u) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok((u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime))
        }

        /// Needs [`Process::open_for_kill`].
        pub(crate) fn terminate(&self, code: u32) -> io::Result<()> {
            // SAFETY: `self.0` is a valid handle opened with `PROCESS_TERMINATE`.
            if unsafe { TerminateProcess(self.0, code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// Whether the process has exited, without waiting. Needs [`Process::open_for_kill`].
        pub(crate) fn has_exited(&self) -> bool {
            // SAFETY: `self.0` is a valid handle opened with `SYNCHRONIZE`.
            unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
        }

        /// Whether the image is the console host a `CREATE_NO_WINDOW` spawn gets.
        fn is_console_host(&self) -> bool {
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            // SAFETY: `buf` is writable for `len` UTF-16 units.
            let ok = unsafe {
                QueryFullProcessImageNameW(self.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
            };
            if ok == 0 {
                return false;
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            let name = path.rsplit(['\\', '/']).next().unwrap_or_default();
            name.eq_ignore_ascii_case("conhost.exe")
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a handle this value owns.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// A Job object the runner owns only to count a run's processes (#37 D2 step 3). It has no
    /// limits; process-wrap's kill-on-close `JobObject` is nested inside it and does the killing.
    pub(crate) struct Job(HANDLE);

    // SAFETY: a job handle may be used from any thread; the queries take no shared state.
    unsafe impl Send for Job {}
    // SAFETY: as above.
    unsafe impl Sync for Job {}

    impl std::fmt::Debug for Job {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Job")
        }
    }

    impl Job {
        pub(crate) fn new() -> io::Result<Self> {
            // SAFETY: null attributes and name create an unnamed, non-inheritable job.
            let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if h.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(h))
        }

        /// Assigns a process, which must still be suspended so all its descendants follow it.
        pub(crate) fn assign(&self, process: HANDLE) -> io::Result<()> {
            // SAFETY: both handles are valid for the call.
            if unsafe { AssignProcessToJobObject(self.0, process) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// Every process in the job and its nested jobs, from the job's own accounting.
        fn pids(&self) -> io::Result<Vec<u32>> {
            // The list's header is two u32s, then `usize` pids.
            let header = 8 / size_of::<usize>();
            let mut cap = 64usize;
            loop {
                let mut buf = vec![0usize; header + cap];
                let bytes = (buf.len() * size_of::<usize>()) as u32;
                let list = buf.as_mut_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
                // SAFETY: `buf` is writable for `bytes` and aligned for the list.
                let ok = unsafe {
                    QueryInformationJobObject(
                        self.0,
                        JobObjectBasicProcessIdList,
                        list.cast(),
                        bytes,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    let err = io::Error::last_os_error();
                    if err.raw_os_error() == Some(ERROR_MORE_DATA as i32) {
                        cap *= 4;
                        continue;
                    }
                    return Err(err);
                }
                // SAFETY: the call filled the header.
                let (assigned, listed) = unsafe {
                    (
                        (*list).NumberOfAssignedProcesses as usize,
                        (*list).NumberOfProcessIdsInList as usize,
                    )
                };
                if assigned > listed {
                    cap = assigned + 16;
                    continue;
                }
                return Ok(buf[header..header + listed]
                    .iter()
                    .filter_map(|p| u32::try_from(*p).ok())
                    .collect());
            }
        }

        /// The job's live processes, without console hosts. An unreadable job counts as empty.
        pub(crate) fn members(&self) -> Vec<u32> {
            self.pids()
                .unwrap_or_default()
                .into_iter()
                .filter(|pid| {
                    Process::open_query(*pid)
                        .map(|p| p.is_running() && !p.is_console_host())
                        .unwrap_or(false)
                })
                .collect()
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a handle this value owns.
            unsafe { CloseHandle(self.0) };
        }
    }
}
