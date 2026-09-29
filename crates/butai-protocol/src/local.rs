//! Local, byte-stream IPC: Unix sockets on Unix, user-private named pipes on Windows.
//! Windows keeps the same socket path as an endpoint identity (and lock-file
//! location); no socket file or TCP listener is created there.

#[cfg(unix)]
pub use tokio::net::{UnixListener as LocalListener, UnixStream as LocalStream};
#[cfg(windows)]
pub use windows::{LocalListener, LocalStream};

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Replay a byte consumed while detecting the protocol, then read the stream.
pub struct Prefixed<S> {
    first: Option<u8>,
    stream: S,
}
impl<S> Prefixed<S> {
    pub fn new(first: u8, stream: S) -> Self {
        Self { first: Some(first), stream }
    }
}
impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if let Some(first) = self.first.take() {
            buf.put_slice(&[first]);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// Construct a helper process without flashing a console window on Windows.
/// Unix process creation and inherited flags stay unchanged.
pub fn background_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    command
}
/// The asynchronous equivalent of [`background_command`].
pub fn background_async_command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut command = tokio::process::Command::new(program);
    #[cfg(windows)]
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    command
}

/// Nonblocking lifetime lock for the daemon. Unix retains its original flock.
pub fn try_lock_exclusive(file: &std::fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(Into::into)
    }
    #[cfg(windows)]
    {
        fs2::FileExt::try_lock_exclusive(file)
    }
}
/// Probe whether a daemon already owns the lock.
pub fn try_lock_shared(file: &std::fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockShared)
            .map_err(Into::into)
    }
    #[cfg(windows)]
    {
        fs2::FileExt::try_lock_shared(file)
    }
}
/// Release a probe lock before starting the daemon.
pub fn unlock(file: &std::fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingUnlock).map_err(Into::into)
    }
    #[cfg(windows)]
    {
        fs2::FileExt::unlock(file)
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::path::{Path, PathBuf};
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };
    use tokio::sync::Mutex;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    pub enum LocalStream {
        Client(NamedPipeClient),
        Server(NamedPipeServer),
    }
    impl LocalStream {
        pub async fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
            let name = pipe_name(path.as_ref())?;
            // A busy server is alive, but all its current instances are in use.
            // Retry briefly so parallel HTTP and pane connections can attach.
            for _ in 0..40 {
                match ClientOptions::new().open(&name) {
                    Ok(client) => return Ok(Self::Client(client)),
                    Err(e) if e.raw_os_error() == Some(231) => {
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await
                    }
                    Err(e) => return Err(e),
                }
            }
            Err(io::Error::new(io::ErrorKind::TimedOut, "named pipe remains busy"))
        }
    }
    impl AsyncRead for LocalStream {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Self::Client(s) => Pin::new(s).poll_read(cx, buf),
                Self::Server(s) => Pin::new(s).poll_read(cx, buf),
            }
        }
    }
    impl AsyncWrite for LocalStream {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            match self.get_mut() {
                Self::Client(s) => Pin::new(s).poll_write(cx, buf),
                Self::Server(s) => Pin::new(s).poll_write(cx, buf),
            }
        }
        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Self::Client(s) => Pin::new(s).poll_flush(cx),
                Self::Server(s) => Pin::new(s).poll_flush(cx),
            }
        }
        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Self::Client(s) => Pin::new(s).poll_shutdown(cx),
                Self::Server(s) => Pin::new(s).poll_shutdown(cx),
            }
        }
    }

    pub struct LocalListener {
        path: PathBuf,
        name: String,
        pending: Mutex<NamedPipeServer>,
    }
    pub struct LocalAddr(PathBuf);
    impl LocalAddr {
        pub fn as_pathname(&self) -> Option<&Path> {
            Some(&self.0)
        }
    }
    impl LocalListener {
        pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
            let path = std::path::absolute(path)?;
            let name = pipe_name(&path)?;
            let pending = Mutex::new(create_server(&name, true)?);
            Ok(Self { path, name, pending })
        }
        pub fn local_addr(&self) -> io::Result<LocalAddr> {
            Ok(LocalAddr(self.path.clone()))
        }
        pub async fn accept(&self) -> io::Result<(LocalStream, LocalAddr)> {
            let mut pending = self.pending.lock().await;
            pending.connect().await?;
            // Keep a listening instance alive before handing off the connected
            // one, so clients never see a missing endpoint between accepts.
            let next = create_server(&self.name, false)?;
            let stream = std::mem::replace(&mut *pending, next);
            Ok((LocalStream::Server(stream), LocalAddr(self.path.clone())))
        }
    }

    fn pipe_name(path: &Path) -> io::Result<String> {
        use std::os::windows::ffi::OsStrExt;
        let absolute = std::path::absolute(path)?;
        let mut hash = Sha256::new();
        for unit in absolute.as_os_str().encode_wide() {
            hash.update(unit.to_le_bytes());
        }
        Ok(format!(r"\\.\pipe\butai-{:x}", hash.finalize()))
    }

    /// Grant access only to the current user and SYSTEM. Windows' default pipe
    /// DACL also grants read access to Everyone; it is unsuitable for this API.
    fn create_server(name: &str, first: bool) -> io::Result<NamedPipeServer> {
        let sid = current_user_sid()?;
        let sddl: Vec<u16> =
            format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})").encode_utf16().chain(Some(0)).collect();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: NUL-terminated SDDL, valid out-pointer. Windows owns the
        // allocated descriptor until LocalFree; create copies it synchronously.
        unsafe {
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let result = ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    name,
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                );
            LocalFree(descriptor);
            result
        }
    }
    fn current_user_sid() -> io::Result<String> {
        // SAFETY: valid token and aligned TOKEN_USER buffer, kept alive through
        // SID conversion. Every allocated handle/string is released below.
        unsafe {
            let mut token = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut size = 0;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
            let mut data = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
            let ok =
                GetTokenInformation(token, TokenUser, data.as_mut_ptr().cast(), size, &mut size);
            let error = io::Error::last_os_error();
            CloseHandle(token);
            if ok == 0 {
                return Err(error);
            }
            let user = &*(data.as_ptr() as *const TOKEN_USER);
            let mut text = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut len = 0;
            while *text.add(len) != 0 {
                len += 1;
            }
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            LocalFree(text.cast());
            Ok(sid)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn protocol_discriminator_is_replayed_once() {
        let (mut writer, reader) = tokio::io::duplex(32);
        writer.write_all(b"ET / HTTP/1.1").await.unwrap();
        writer.shutdown().await.unwrap();
        let mut reader = Prefixed::new(b'G', reader);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"GET / HTTP/1.1");
    }

    #[tokio::test]
    async fn local_listener_handles_multiple_clients_and_releases_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sock");
        let listener = LocalListener::bind(&path).unwrap();
        assert!(LocalListener::bind(&path).is_err());
        for byte in [0, b'G', 42] {
            let mut client = LocalStream::connect(&path).await.unwrap();
            let (mut server, _) = listener.accept().await.unwrap();
            client.write_all(&[byte]).await.unwrap();
            assert_eq!(server.read_u8().await.unwrap(), byte);
            server.write_all(&[byte + 1]).await.unwrap();
            assert_eq!(client.read_u8().await.unwrap(), byte + 1);
        }
        drop(listener);
        assert!(LocalStream::connect(&path).await.is_err());
        // Unix retains the socket inode after closing; Windows has no file.
        let _ = std::fs::remove_file(&path);
        assert!(LocalListener::bind(&path).is_ok());
    }
}
