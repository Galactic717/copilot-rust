//! Child processes (whisper-server, llama-server): hidden console, and tied to a kill-on-close job object so they
//! never outlive the app, even after a crash.

use std::process::{Child, Command};
use std::sync::OnceLock;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

struct Job(HANDLE);
// The job handle is only used for AssignProcessToJobObject, which is thread-safe.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

fn job() -> Option<&'static Job> {
    static JOB: OnceLock<Option<Job>> = OnceLock::new();
    JOB.get_or_init(|| unsafe {
        let h = CreateJobObjectW(None, None).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            h,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .ok()?;
        Some(Job(h))
    })
    .as_ref()
}

pub fn spawn_hidden(cmd: &mut Command) -> std::io::Result<Child> {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    let child = cmd.creation_flags(CREATE_NO_WINDOW).spawn()?;
    if let Some(j) = job() {
        unsafe {
            let _ = AssignProcessToJobObject(j.0, HANDLE(child.as_raw_handle()));
        }
    }
    Ok(child)
}

// whisper-server and llama-server are built against the VC++ runtime. build.rs embeds its DLLs, so the sidecars start on
// a clean Windows without the redistributable installed. Next to the exe wins over System32 only when System32 has none.
include!(concat!(env!("OUT_DIR"), "/vcrt.rs"));

/// Puts the runtime DLLs beside a sidecar. A failure is only logged: most machines already have the runtime.
pub fn ensure_vc_runtime(dir: &std::path::Path) {
    for (name, bytes) in VCRT {
        let p = dir.join(name);
        if !p.is_file() {
            if let Err(e) = std::fs::write(&p, bytes) {
                crate::db::diag("vcrt_write_failed", serde_json::json!({ "file": name, "error": e.to_string() }));
            }
        }
    }
}

pub fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}
