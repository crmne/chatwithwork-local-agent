//! Control transport for Windows: a named pipe that only the current user
//! can open.
//!
//! - The pipe is created with a DACL that grants access to the current
//!   user's SID and nobody else, and with remote clients rejected.
//! - `first_pipe_instance` makes a second daemon (or a squatter that got
//!   there first) fail loudly instead of sharing the name.
//! - The daemon identifies each client by impersonating it for a moment
//!   (identification level only) and refuses any other user. The client
//!   checks that the pipe is owned by its own user before sending anything,
//!   and opens it at identification level, so a server can never act as it.

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
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

use crate::win;

pub type ClientStream = File;

/// A security descriptor from `ConvertStringSecurityDescriptorToSecurityDescriptorW`.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// The descriptor is immutable after creation and only read by the kernel.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    fn for_sid(sid: &str) -> Result<Self> {
        // Owned by this SID (even from an elevated shell, where the owner
        // would otherwise be Administrators), protected DACL, generic-all
        // for this SID only.
        let sddl = win::wide(&format!("O:{sid}D:P(A;;GA;;;{sid})"));
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
        match win::pipe_client_sid(&connected) {
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
    // Only the creator (or an administrator) can own the pipe, so its owner
    // tells whose daemon this is.
    let own = win::current_user_sid()?;
    let theirs =
        win::object_owner_sid(&file).with_context(|| format!("checking who owns {name}"))?;
    if theirs != own {
        bail!("{name} belongs to another user ({theirs}); refusing to talk to it");
    }
    Ok(Some(file))
}

/// Pipe reads have no timeout on Windows; nothing to clear.
pub fn clear_timeout(_stream: &ClientStream) {}
