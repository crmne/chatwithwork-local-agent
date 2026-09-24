//! Control transport for Windows: a named pipe that only the current user
//! can open.
//!
//! - The pipe is created with a DACL that grants access to the current
//!   user's SID and nobody else, and with remote clients rejected.
//! - `first_pipe_instance` makes a second daemon (or a squatter that got
//!   there first) fail loudly instead of sharing the name.
//! - The daemon checks that each client process runs as the same user, and
//!   the client checks the same of the server before sending anything. The
//!   client also opens the pipe at identification level, so a server can
//!   never impersonate it.

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};

use crate::win;

pub type ClientStream = File;

/// A security descriptor from `ConvertStringSecurityDescriptorToSecurityDescriptorW`.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// The descriptor is immutable after creation and only read by the kernel.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    fn for_sid(sid: &str) -> Result<Self> {
        // Protected DACL, generic-all for this SID only.
        let sddl = win::wide(&format!("D:P(A;;GA;;;{sid})"));
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).context("building the pipe's security");
        }
        Ok(Self(sd))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

pub struct Listener {
    name: String,
    sd: SecurityDescriptor,
    sid: String,
    next: NamedPipeServer,
}

fn pipe_name(path: &Path) -> Result<String> {
    let name = path.to_str().context("pipe name is not UTF-8")?;
    if !name.starts_with(r"\\.\pipe\") {
        bail!("{name} is not a named pipe path");
    }
    Ok(name.to_string())
}

fn create(name: &str, sd: &SecurityDescriptor, first: bool) -> std::io::Result<NamedPipeServer> {
    let mut sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, &mut sa as *mut _ as *mut c_void)
    }
}

pub fn bind(path: &Path) -> Result<Listener> {
    let name = pipe_name(path)?;
    let sid = win::current_user_sid()?;
    let sd = SecurityDescriptor::for_sid(&sid)?;
    let next = match create(&name, &sd, true) {
        Ok(server) => server,
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            bail!("another cww daemon is already running ({name})")
        }
        Err(e) => return Err(e).with_context(|| format!("creating {name}")),
    };
    Ok(Listener {
        name,
        sd,
        sid,
        next,
    })
}

impl Listener {
    /// The next connection from a process of the same user.
    pub async fn accept(&mut self) -> Result<NamedPipeServer> {
        self.next.connect().await?;
        let fresh = create(&self.name, &self.sd, false)
            .with_context(|| format!("creating {}", self.name))?;
        let connected = std::mem::replace(&mut self.next, fresh);
        let mut pid = 0u32;
        let ok = unsafe { GetNamedPipeClientProcessId(connected.as_raw_handle() as _, &mut pid) };
        if ok == 0 {
            bail!("can't identify the control client");
        }
        match win::process_user_sid(pid) {
            Ok(sid) if sid == self.sid => Ok(connected),
            Ok(sid) => bail!("refused a connection from {sid}"),
            Err(e) => Err(e).context("checking the control client"),
        }
    }
}

pub fn connect(path: &Path) -> Result<Option<ClientStream>> {
    let name = pipe_name(path)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let file = loop {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(&name)
        {
            Ok(file) => break file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                if Instant::now() > deadline {
                    return Err(e).context("the daemon is busy");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e).with_context(|| format!("connecting to {name}")),
        }
    };
    let mut pid = 0u32;
    let ok = unsafe { GetNamedPipeServerProcessId(file.as_raw_handle() as _, &mut pid) };
    if ok == 0 {
        bail!("can't identify the process behind {name}");
    }
    let own = win::current_user_sid()?;
    let theirs = win::process_user_sid(pid).context("checking the daemon's user")?;
    if theirs != own {
        bail!("{name} belongs to another user ({theirs}); refusing to talk to it");
    }
    Ok(Some(file))
}

/// Pipe reads have no timeout on Windows; nothing to clear.
pub fn clear_timeout(_stream: &ClientStream) {}
