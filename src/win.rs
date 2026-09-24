//! Small Windows helpers: wide strings and user SIDs.

use std::io;

use anyhow::{Context, Result};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
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

/// The string SID of the user process `pid` runs as.
pub fn process_user_sid(pid: u32) -> Result<String> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error()).with_context(|| format!("opening process {pid}"));
    }
    let process = OwnedHandle(process);
    token_user_sid(process.0).with_context(|| format!("reading the user of process {pid}"))
}

fn token_user_sid(process: HANDLE) -> Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let token = OwnedHandle(token);
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
    let mut string: *mut u16 = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut string) } == 0 {
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
