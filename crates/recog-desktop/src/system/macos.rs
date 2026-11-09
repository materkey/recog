//! psutil's macOS process table. The kernel's `kinfo_proc` (sysctl `KERN_PROC_PID`)
//! gives what `psutil/_psosx.py` takes from it: the short name, the real uid and the
//! start time to the microsecond (`tv_sec + tv_usec / 1e6`, the target's identity);
//! `proc_pidpath` the executable; `KERN_PROCARGS2` (through sysinfo) the arguments
//! and environment; the sockets come from libproc's per-descriptor socket info
//! (through listeners).

use std::ffi::OsString;

use libproc::libproc::proc_pid;
use libproc::processes::{ProcFilter, pids_by_type};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::host::ProcessTable;
use crate::py::PyErr;

/// The table of this Mac.
pub struct MacTable;

/// The names XNU gives the strings it appends after a process's environment
/// (`exec_add_apple_strings`).
const APPLE_STRINGS: [&str; 17] = [
    "executable_path",
    "pfz",
    "stack_guard",
    "malloc_entropy",
    "ptr_munge",
    "main_stack",
    "executable_file",
    "dyld_file",
    "executable_cdhash",
    "executable_boothash",
    "th_port",
    "arm64e_abi",
    "dyld_flags",
    "subsystem_root_path",
    "has_sec_transition",
    "security_config",
    "apple_dyld_flags",
];

fn raw_pid(pid: i64) -> Result<i32, PyErr> {
    i32::try_from(pid).map_err(|_| PyErr::no_such_process(pid))
}

fn kinfo(pid: i64) -> Result<libproc::libproc::kinfo::KinfoProc, PyErr> {
    let raw = raw_pid(pid)?;
    let info = proc_pid::kproc_info_raw(raw).map_err(|_| PyErr::no_such_process(pid))?;
    // sysctl answers a vanished pid with an empty record rather than an error.
    if info.kp_proc.p_pid != raw {
        return Err(PyErr::no_such_process(pid));
    }
    Ok(info)
}

/// `KERN_PROCARGS2` of one process: its arguments and environment block.
fn procargs(pid: i64) -> Result<(Vec<String>, Vec<OsString>), PyErr> {
    let raw = u32::try_from(pid).map_err(|_| PyErr::no_such_process(pid))?;
    let mut system = System::new().map_err(|_| PyErr::access_denied(pid))?;
    let kind = ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always).with_environ(UpdateKind::Always);
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[Pid::from_u32(raw)]), true, kind);
    let process = system.process(Pid::from_u32(raw)).ok_or_else(|| PyErr::no_such_process(pid))?;
    let args = process.cmd().iter().map(|a| a.to_string_lossy().into_owned()).collect();
    Ok((args, process.environ().to_vec()))
}

impl ProcessTable for MacTable {
    fn pids(&self) -> Vec<i64> {
        let mut pids: Vec<i64> = pids_by_type(ProcFilter::All).unwrap_or_default().into_iter().map(i64::from).collect();
        pids.sort_unstable();
        pids.dedup();
        pids
    }

    fn open(&self, pid: i64) -> Result<(), PyErr> {
        if pid < 0 {
            return Err(PyErr::value(format!("pid must be a positive integer (got {pid})")));
        }
        kinfo(pid).map(|_| ())
    }

    fn name(&self, pid: i64) -> Result<String, PyErr> {
        let info = kinfo(pid)?;
        let bytes: Vec<u8> = info.kp_proc.p_comm.iter().take_while(|&&c| c != 0).map(|&c| c.to_le_bytes()[0]).collect();
        let name = String::from_utf8_lossy(&bytes).into_owned();
        let cmdline = if name.chars().count() >= 15 { procargs(pid).ok().map(|(args, _)| args) } else { None };
        Ok(super::extend_name(name, cmdline.as_deref()))
    }

    fn uid_real(&self, pid: i64) -> Result<u32, PyErr> {
        Ok(kinfo(pid)?.kp_eproc.e_pcred.p_ruid)
    }

    fn create_time(&self, pid: i64) -> Result<f64, PyErr> {
        let start = kinfo(pid)?.kp_proc.p_un;
        #[allow(clippy::cast_precision_loss)]
        Ok(start.tv_sec as f64 + f64::from(start.tv_usec) / 1_000_000.0)
    }

    fn exe(&self, pid: i64) -> Result<String, PyErr> {
        match proc_pid::pidpath(raw_pid(pid)?) {
            Ok(path) => Ok(path),
            Err(_) => {
                kinfo(pid)?;
                Err(PyErr::access_denied(pid))
            }
        }
    }

    fn cmdline(&self, pid: i64) -> Result<Vec<String>, PyErr> {
        kinfo(pid)?;
        Ok(procargs(pid)?.0)
    }

    fn environ(&self, pid: i64) -> Result<Vec<(String, String)>, PyErr> {
        kinfo(pid)?;
        let mut entries = procargs(pid)?.1;
        // sysinfo reads past the empty string that ends the environment, into the
        // kernel's "apple" strings for dyld; psutil stops there.
        while entries.last().is_some_and(|e| {
            let entry = e.to_string_lossy();
            entry.split_once('=').is_some_and(|(key, _)| APPLE_STRINGS.contains(&key))
        }) {
            entries.pop();
        }
        let mut block = String::new();
        for entry in entries {
            block.push_str(&entry.to_string_lossy());
            block.push('\0');
        }
        block.push('\0');
        Ok(super::parse_environ_block(&block))
    }

    fn tcp_listeners(&self, pid: i64) -> Result<Vec<(String, u16)>, PyErr> {
        kinfo(pid)?;
        let all = listeners::get_all().map_err(|_| PyErr::access_denied(pid))?;
        Ok(all
            .into_iter()
            .filter(|l| {
                i64::from(l.process.pid) == pid
                    && l.protocol == listeners::Protocol::TCP
                    && l.state == listeners::SocketState::Listen
            })
            .map(|l| (l.socket.ip().to_string(), l.socket.port()))
            .collect())
    }
}
