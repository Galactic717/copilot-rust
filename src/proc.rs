//! Child processes (whisper-server, llama-server): hidden console, and tied to a kill-on-close job object so they
//! never outlive the app, even after a crash.

use std::io::{self, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct Job(HANDLE);
// The job handle is only used for AssignProcessToJobObject, which is thread-safe.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            let job = Self(CreateJobObjectW(None, None)?);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(job.0, JobObjectExtendedLimitInformation, (&raw const info).cast(), std::mem::size_of_val(&info) as u32)?;
            Ok(job)
        }
    }

    fn attach(&self, child: &Child) -> windows::core::Result<()> {
        unsafe { AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle())) }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub fn spawn_hidden(cmd: &mut Command) -> std::io::Result<Child> {
    static JOB: OnceLock<Option<Job>> = OnceLock::new();
    let child = cmd.creation_flags(CREATE_NO_WINDOW).spawn()?;
    if let Some(job) = JOB.get_or_init(|| Job::new().ok()) {
        let _ = job.attach(&child);
    }
    Ok(child)
}

pub fn spawn_batch(path: &Path) -> io::Result<(Child, Job)> {
    let job = Job::new()?;
    let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let mut child = Command::new(system.join("System32").join("cmd.exe"))
        .args(["/D", "/Q", "/V:OFF"])
        .env("COPILOT_BATCH_PATH", path)
        .current_dir(path.parent().expect("absolute batch path"))
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Hold the shell at stdin until its children are guaranteed to inherit this job.
    let start = job.attach(&child).map_err(io::Error::other).and_then(|()| {
        child.stdin.take().expect("piped shell stdin").write_all(b"\"%COPILOT_BATCH_PATH%\" --no-pause\r\nexit /b %ERRORLEVEL%\r\n")
    });
    if let Err(e) = start {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    Ok((child, job))
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
