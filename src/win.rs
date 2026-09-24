//! Small Windows helpers: wide strings and user SIDs.

use std::io;
use std::os::windows::io::AsRawHandle;

use anyhow::{Context, Result};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, RevertToSelf,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
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

/// The string SID (`S-1-5-21-...`) of the user this process runs as.
pub fn current_user_sid() -> Result<String> {
    token_user_sid(unsafe { GetCurrentProcess() }).context("reading this process's user")
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
    let mut owner: PSID = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle() as HANDLE,
            SE_KERNEL_OBJECT,
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
