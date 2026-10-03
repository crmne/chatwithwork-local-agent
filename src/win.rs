//! Small Windows helpers: wide strings, user SIDs, and who owns and may
//! use a file.

use std::io;
use std::os::windows::io::AsRawHandle;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT, SE_KERNEL_OBJECT, SE_OBJECT_TYPE,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, GetTokenInformation,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, RevertToSelf, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
};

/// A NUL-terminated UTF-16 copy of `s`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// `NT AUTHORITY\SYSTEM`.
pub const SYSTEM_SID: &str = "S-1-5-18";
/// `BUILTIN\Administrators`.
pub const ADMINISTRATORS_SID: &str = "S-1-5-32-544";

/// The string SID (`S-1-5-21-...`) of the user this process runs as.
pub fn current_user_sid() -> Result<String> {
    token_user_sid(unsafe { GetCurrentProcess() }).context("reading this process's user")
}

/// [`current_user_sid`], read once.
pub fn user_sid() -> Option<&'static str> {
    static SID: OnceLock<Option<String>> = OnceLock::new();
    SID.get_or_init(|| current_user_sid().ok()).as_deref()
}

/// What an allow entry of a DACL grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub sid: String,
    pub mask: u32,
}

/// Who owns a file or folder and what its DACL allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSecurity {
    pub owner: String,
    /// `None` for a null DACL, which allows everyone everything.
    pub grants: Option<Vec<Grant>>,
    /// The DACL has entries other than allow and deny ones (object or
    /// callback entries), which this check doesn't read.
    pub unusual: bool,
}

/// The owner SID of an open file or folder (opened with `READ_CONTROL`).
pub fn file_owner_sid(handle: &impl AsRawHandle) -> Result<String> {
    owner_of(handle.as_raw_handle() as HANDLE, SE_FILE_OBJECT)
}

/// The owner and the DACL of an open file or folder (opened with
/// `READ_CONTROL`).
pub fn file_security(handle: &impl AsRawHandle) -> Result<FileSecurity> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32)).context("reading the security");
    }
    let read = (|| {
        let owner = sid_string(owner)?;
        if dacl.is_null() {
            return Ok(FileSecurity {
                owner,
                grants: None,
                unusual: false,
            });
        }
        let mut grants = Vec::new();
        let mut unusual = false;
        // SAFETY: `dacl` points into `sd`, which lives until LocalFree.
        let count = unsafe { (*dacl).AceCount };
        for i in 0..u32::from(count) {
            let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
            if unsafe { GetAce(dacl, i, &mut ace) } == 0 {
                return Err(io::Error::last_os_error().into());
            }
            let header = unsafe { &*(ace as *const ACE_HEADER) };
            match header.AceType {
                // ACCESS_ALLOWED_ACE_TYPE
                0 => {
                    let allowed = ace as *const ACCESS_ALLOWED_ACE;
                    let sid = unsafe { std::ptr::addr_of!((*allowed).SidStart) } as PSID;
                    grants.push(Grant {
                        sid: sid_string(sid)?,
                        mask: unsafe { (*allowed).Mask },
                    });
                }
                // Deny entries only take rights away; audit and mandatory
                // label entries don't belong in a DACL's grants.
                1 | 2 | 0x11 => {}
                _ => unusual = true,
            }
        }
        Ok(FileSecurity {
            owner,
            grants: Some(grants),
            unusual,
        })
    })();
    unsafe {
        LocalFree(sd);
    }
    read
}

fn token_user_sid(process: HANDLE) -> Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    user_of_token(&OwnedHandle(token))
}

/// The string SID of the process at the other end of a named pipe, found by
/// briefly impersonating it. The client must allow at least identification
/// (`SECURITY_IDENTIFICATION`). This works across logon sessions, where
/// opening the client's process would be denied.
pub fn pipe_client_sid(pipe: &impl AsRawHandle) -> Result<String> {
    if unsafe { ImpersonateNamedPipeClient(pipe.as_raw_handle() as HANDLE) } == 0 {
        return Err(io::Error::last_os_error()).context("identifying the pipe client");
    }
    let mut token: HANDLE = std::ptr::null_mut();
    // OpenAsSelf: open the thread token with the daemon's own rights.
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    let error = io::Error::last_os_error();
    // Stop impersonating before anything else happens on this thread.
    if unsafe { RevertToSelf() } == 0 {
        // Carrying on as someone else would be worse than stopping.
        std::process::abort();
    }
    if opened == 0 {
        return Err(error).context("reading the pipe client's token");
    }
    user_of_token(&OwnedHandle(token))
}

/// The string SID that owns a kernel object, such as a named pipe.
pub fn object_owner_sid(handle: &impl AsRawHandle) -> Result<String> {
    owner_of(handle.as_raw_handle() as HANDLE, SE_KERNEL_OBJECT)
}

fn owner_of(handle: HANDLE, kind: SE_OBJECT_TYPE) -> Result<String> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle,
            kind,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32)).context("reading the owner");
    }
    let sid = sid_string(owner);
    unsafe {
        LocalFree(sd);
    }
    sid
}

fn user_of_token(token: &OwnedHandle) -> Result<String> {
    let mut needed = 0u32;
    unsafe {
        GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed);
    }
    // u64 elements keep the buffer aligned for TOKEN_USER.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    sid_string(user.User.Sid)
}

fn sid_string(sid: PSID) -> Result<String> {
    let mut string: *mut u16 = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut string) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let len = (0..)
        .take_while(|&i| unsafe { *string.add(i) } != 0)
        .count();
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(string, len) });
    unsafe {
        LocalFree(string.cast());
    }
    Ok(sid)
}
