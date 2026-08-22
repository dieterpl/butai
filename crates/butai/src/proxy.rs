//! `butai proxy`: bridge stdin/stdout to the daemon socket.
//!
//! This is the remote-access path: a GUI or script runs `ssh host butai
//! proxy` and speaks the length-prefixed JSON protocol over ssh's stdio —
//! SSH provides both the transport and the authentication, and the daemon
//! never listens on TCP.

use anyhow::Result;
use tokio::io::AsyncWriteExt;

pub fn run(socket: &std::path::Path) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let socket = socket.to_path_buf();
    let result = rt.block_on(async move {
        let stream = butai_client::conn::connect_or_spawn(&socket).await?;
        let (mut sock_read, mut sock_write) = stream.into_split();
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();

        // stdin -> daemon. When stdin ends we half-close the socket rather than
        // tearing the whole bridge down, so a one-shot `butai proxy < request`
        // still gets to read its reply.
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stdin, &mut sock_write).await;
            let _ = sock_write.shutdown().await;
        });

        // daemon -> stdout, until the daemon closes. That's our exit condition:
        // for HTTP it's the response being complete, for the framed protocol
        // it's the session ending.
        let copied = tokio::io::copy(&mut sock_read, &mut stdout).await;
        stdout.flush().await?;
        copied?;
        Ok::<(), anyhow::Error>(())
    });

    // Never *drop* the runtime: its shutdown waits for blocking tasks, and the
    // one reading stdin is parked in read(2) until our peer closes the pipe. Over
    // ssh that only happens when the client gives up, so dropping here stranded
    // the process — and every REST request paid the client's full timeout for a
    // reply it already had. Shut down without waiting instead.
    rt.shutdown_background();
    result
}
