//! One app per user. A second launch (from the launcher, or at login while
//! the app already runs) asks the first to open its settings window, then
//! exits.
//!
//! The first instance listens on a socket next to the daemon's (a named
//! pipe on Windows), blocked in accept until someone knocks.

use std::io::Write;
#[cfg(unix)]
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::events::{AppEvent, Events};

pub enum Instance {
    /// This process is the app. Call [`Primary::listen`] once events flow.
    Primary(Primary),
    /// Another instance was already running and has been told.
    Secondary,
}

#[cfg(unix)]
pub struct Primary(std::os::unix::net::UnixListener);

#[cfg(windows)]
pub struct Primary(std::path::PathBuf, windows::Pipe);

/// Claim the instance endpoint, or knock on the running instance. With
/// `show`, the running instance opens its settings window.
pub fn claim(path: &Path, show: bool) -> std::io::Result<Instance> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        use std::os::unix::net::{UnixListener, UnixStream};

        if let Some(dir) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .create(dir)
                .and_then(|()| {
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                })?;
        }
        // Launches claim one at a time. Otherwise two could both find
        // nobody listening, and the second would remove the socket the first
        // just bound and bind its own: two primaries. Under the lock, the
        // second finds the first listening. The kernel drops the lock if the
        // process dies, so a crash never leaves it held.
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let claiming = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&lock_path)?;
        claiming.lock()?;

        for _ in 0..3 {
            match UnixStream::connect(path) {
                Ok(mut stream) => {
                    let _ = stream.write_all(if show { b"show\n" } else { b"ping\n" });
                    return Ok(Instance::Secondary);
                }
                // Nobody is listening: no file, or one left by a crash.
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) => {}
                // Anything else (permissions, resources) says nothing about
                // whether an instance runs, so never replace its socket.
                Err(e) => return Err(e),
            }
            // Nobody is listening, so a file left there is stale.
            let _ = std::fs::remove_file(path);
            match UnixListener::bind(path) {
                Ok(listener) => {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
                    return Ok(Instance::Primary(Primary(listener)));
                }
                // Bound between our connect and bind by an app that didn't
                // take the lock.
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::other("couldn't claim the app's socket"))
    }
    #[cfg(windows)]
    {
        match windows::Pipe::create(path, true) {
            Ok(pipe) => Ok(Instance::Primary(Primary(path.to_path_buf(), pipe))),
            Err(_) => {
                let mut pipe = std::fs::OpenOptions::new().write(true).open(path)?;
                let _ = pipe.write_all(if show { b"show\n" } else { b"ping\n" });
                Ok(Instance::Secondary)
            }
        }
    }
}

impl Primary {
    /// Answer knocks for as long as the process runs.
    pub fn listen(self, events: Events) {
        let spawned = std::thread::Builder::new()
            .name("cww-instance".into())
            .spawn(move || self.serve(&events));
        if let Err(e) = spawned {
            log::warn!("single-instance listener: {e}");
        }
    }

    #[cfg(unix)]
    fn serve(self, events: &Events) {
        for stream in self.0.incoming().flatten() {
            let mut line = String::new();
            let _ = BufReader::new(stream).read_line(&mut line);
            if line.trim() == "show" {
                events.send(AppEvent::OpenSettings);
            }
        }
    }

    #[cfg(windows)]
    fn serve(self, events: &Events) {
        let Primary(path, mut pipe) = self;
        loop {
            if let Some(line) = pipe.accept_line()
                && line.trim() == "show"
            {
                events.send(AppEvent::OpenSettings);
            }
            match windows::Pipe::create(&path, false) {
                Ok(next) => pipe = next,
                Err(e) => {
                    log::warn!("single-instance pipe: {e}");
                    return;
                }
            }
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND, ReadFile,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    pub struct Pipe(HANDLE);

    // SAFETY: a pipe handle can be used from any thread.
    unsafe impl Send for Pipe {}

    /// Create one server instance of the pipe `path`.
    fn create_raw(path: &Path, first: bool) -> std::io::Result<HANDLE> {
        let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let flags = PIPE_ACCESS_INBOUND
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
        // SAFETY: `name` is NUL-terminated; default security (the creator
        // and administrators) applies.
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                flags,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        Ok(handle)
    }

    /// Block until a client connects to this instance.
    fn connect_raw(handle: HANDLE) -> std::io::Result<()> {
        // SAFETY: the handle is a pipe we created.
        if unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        // ERROR_PIPE_CONNECTED: the client connected first.
        if e.raw_os_error() == Some(535) {
            Ok(())
        } else {
            Err(e)
        }
    }

    impl Pipe {
        pub fn create(path: &Path, first: bool) -> std::io::Result<Self> {
            create_raw(path, first).map(Self)
        }

        /// Block until a client connects, and read what it says.
        pub fn accept_line(&mut self) -> Option<String> {
            connect_raw(self.0).ok()?;
            let mut buf = [0u8; 64];
            let mut read = 0u32;
            // SAFETY: `buf` outlives the call and its length is passed.
            let ok = unsafe {
                ReadFile(
                    self.0,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut read,
                    std::ptr::null_mut(),
                )
            } != 0;
            // SAFETY: as above.
            unsafe { DisconnectNamedPipe(self.0) };
            ok.then(|| String::from_utf8_lossy(&buf[..read as usize]).into_owned())
        }
    }

    impl Drop for Pipe {
        fn drop(&mut self) {
            // SAFETY: we own the handle.
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::events::Waker;

    #[test]
    fn a_second_launch_knocks_on_the_first() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("run/app.sock");
        let Instance::Primary(primary) = claim(&path, true).unwrap() else {
            panic!("the first launch is the primary");
        };
        let (events, rx) = Events::new(Waker::new(|| {}));
        primary.listen(events);
        assert!(matches!(claim(&path, false).unwrap(), Instance::Secondary));
        assert!(matches!(claim(&path, true).unwrap(), Instance::Secondary));
        let event = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(
            event,
            AppEvent::OpenSettings,
            "only `show` opens the window"
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_socket_it_cannot_reach_is_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("app.sock");
        let live = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // root can connect anyway; the check needs an ordinary user.
        if std::os::unix::net::UnixStream::connect(&path).is_ok() {
            return;
        }
        let err = claim(&path, true).err().expect("an error, not a claim");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(path.exists(), "the live socket is still there");
        drop(live);
    }

    #[test]
    fn simultaneous_launches_make_one_primary() {
        for _ in 0..20 {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("app.sock");
            // A crash left the socket: every launch finds nobody listening.
            drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
            let launches: Vec<_> = (0..4)
                .map(|_| {
                    let (path, barrier) = (path.clone(), std::sync::Arc::clone(&barrier));
                    std::thread::spawn(move || {
                        barrier.wait();
                        claim(&path, false).unwrap()
                    })
                })
                .collect();
            let primaries: Vec<Primary> = launches
                .into_iter()
                .filter_map(|l| match l.join().unwrap() {
                    Instance::Primary(p) => Some(p),
                    Instance::Secondary => None,
                })
                .collect();
            assert_eq!(primaries.len(), 1);
        }
    }

    #[test]
    fn stale_sockets_are_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("app.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists(), "a crashed instance left its socket");
        assert!(matches!(claim(&path, true).unwrap(), Instance::Primary(_)));
    }
}
