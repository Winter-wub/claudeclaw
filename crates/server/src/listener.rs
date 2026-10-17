//! The local listener (architecture §4, §9.3).
//!
//! On Unix, the socket is bound at a temp name in the same directory, `chmod 0600`, then renamed
//! into place, so a client never finds a socket with wider permissions. A stale socket file is
//! removed; a live one is `AddrInUse`. On Windows, an owner-only named pipe.

use std::io;
use std::path::Path;

/// Binds the local socket or pipe at `path`, ready to serve.
///
/// On Unix `path` is a socket path; on Windows it is a pipe name (`\\.\pipe\…`).
#[cfg(unix)]
pub async fn bind_local(path: &Path) -> io::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // A socket that answers is already in use; anything else at the path is stale.
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("{} is already in use", path.display()),
        ));
    }
    remove_if_present(path)?;

    let temp = temp_path(path);
    remove_if_present(&temp)?;
    let listener = tokio::net::UnixListener::bind(&temp)?;
    // Narrow the permissions before the name is visible to anyone else.
    std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&temp, path)?;
    Ok(listener)
}

#[cfg(unix)]
fn temp_path(path: &Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    std::path::PathBuf::from(name)
}

#[cfg(unix)]
fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
pub use windows::PipeListener;

/// Binds the owner-only named pipe at `name`.
#[cfg(windows)]
pub async fn bind_local(name: &str) -> io::Result<PipeListener> {
    PipeListener::bind(name)
}

/// The Windows named-pipe listener: `first_pipe_instance(true)`, `reject_remote_clients(true)` and
/// an owner-only DACL, as architecture §4 requires.
#[cfg(windows)]
mod windows {
    use std::io;
    use std::ptr;

    use axum::serve::Listener;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    /// A named-pipe listener whose instances only the owner may open.
    pub struct PipeListener {
        path: String,
        /// A leaked `SECURITY_ATTRIBUTES`, valid for the process lifetime.
        attributes: usize,
        /// The next instance to connect; pre-created so a client always has one to reach.
        pending: Option<NamedPipeServer>,
    }

    impl PipeListener {
        pub fn bind(name: &str) -> io::Result<Self> {
            let attributes = owner_only_attributes()?;
            let pending = Self::create(name, attributes, true)?;
            Ok(PipeListener {
                path: name.to_owned(),
                attributes,
                pending: Some(pending),
            })
        }

        fn create(name: &str, attributes: usize, first: bool) -> io::Result<NamedPipeServer> {
            let mut options = ServerOptions::new();
            options.first_pipe_instance(first);
            options.reject_remote_clients(true);
            // SAFETY: `attributes` points at a leaked `SECURITY_ATTRIBUTES` that lives for the
            // whole process, exactly what `create_with_security_attributes_raw` requires.
            unsafe { options.create_with_security_attributes_raw(name, attributes as *mut _) }
        }

        fn next_instance(&self) -> io::Result<NamedPipeServer> {
            Self::create(&self.path, self.attributes, false)
        }
    }

    impl Listener for PipeListener {
        type Io = NamedPipeServer;
        type Addr = ();

        async fn accept(&mut self) -> (Self::Io, Self::Addr) {
            loop {
                let Some(server) = self.pending.take() else {
                    match self.next_instance() {
                        Ok(next) => self.pending = Some(next),
                        Err(error) => {
                            tracing::warn!(%error, "named pipe listener could not create an instance");
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                    }
                    continue;
                };
                match server.connect().await {
                    Ok(()) => {
                        if let Ok(next) = self.next_instance() {
                            self.pending = Some(next);
                        }
                        return (server, ());
                    }
                    Err(error) => {
                        tracing::warn!(%error, "named pipe listener accept failed");
                        if let Ok(next) = self.next_instance() {
                            self.pending = Some(next);
                        }
                    }
                }
            }
        }

        fn local_addr(&self) -> io::Result<Self::Addr> {
            Ok(())
        }
    }

    /// A `SECURITY_ATTRIBUTES` whose DACL grants only the owner. `D:P(A;;GA;;;OW)`: a protected
    /// DACL with one access-allowed ACE giving the owner full access. The descriptor is leaked on
    /// purpose, so it stays valid for every instance for the process lifetime (the
    /// `Win32_System_Memory` feature that would free it is not enabled).
    fn owner_only_attributes() -> io::Result<usize> {
        let sddl: Vec<u16> = "D:P(A;;GA;;;OW)\0".encode_utf16().collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `sddl` is a valid NUL-terminated UTF-16 string and `descriptor` is a valid
        // out-pointer for the call.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let attributes = Box::new(SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        });
        // Intentionally leaked.
        Ok(Box::into_raw(attributes) as usize)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn a_bound_socket_is_0600_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api.sock");
        let listener = bind_local(&path).await.unwrap();
        assert!(path.exists());
        assert_eq!(mode(&path), 0o600);
        assert!(!temp_path(&path).exists());
        drop(listener);
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api.sock");
        std::fs::write(&path, b"stale").unwrap();
        let listener = bind_local(&path).await.unwrap();
        assert!(path.exists());
        assert_eq!(mode(&path), 0o600);
        drop(listener);
    }

    #[tokio::test]
    async fn a_live_socket_is_addr_in_use() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api.sock");
        let _listener = bind_local(&path).await.unwrap();
        let error = bind_local(&path).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
    }
}
