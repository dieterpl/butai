//! Private HTTP adapter for the packaged Windows browser bridge.
use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub fn run(socket: &std::path::Path) -> Result<()> {
    let token = std::env::var("BUTAI_WEB_GATEWAY_TOKEN").context("missing web gateway token")?;
    anyhow::ensure!(
        token.len() >= 32 && token.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid web gateway token"
    );
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let result = rt.block_on(async {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        println!("{}", listener.local_addr()?.port());
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut stdin = tokio::io::stdin();
        let mut stop = [0u8; 1];
        loop {
            let accepted = tokio::select! {
                result = listener.accept() => result,
                _ = stdin.read(&mut stop) => break,
            };
            let (mut client, _) = accepted?;
            let socket = socket.to_path_buf();
            let token = token.clone();
            tokio::spawn(async move {
                let result = async {
                    let mut header = Vec::new();
                    // Read exactly the headers so no request-body bytes are lost.
                    tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    while !header.ends_with(b"\r\n\r\n") {
                        anyhow::ensure!(header.len() < 16384, "request headers too large");
                        header.push(client.read_u8().await?);
                    }
                    Ok::<(), anyhow::Error>(())
                    }).await??;
                    let text = std::str::from_utf8(&header)?;
                    let authenticated = text.split("\r\n").skip(1).any(|line| {
                        line.split_once(':').is_some_and(|(name, value)| {
                            name.eq_ignore_ascii_case("x-butai-web-token") && value.trim() == token
                        })
                    });
                    if !authenticated {
                        client.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
                        return Ok::<(), anyhow::Error>(());
                    }
                    let mut daemon = butai_client::conn::connect_or_spawn(&socket).await?;
                    daemon.write_all(&header).await?;
                    tokio::io::copy_bidirectional(&mut client, &mut daemon).await?;
                    Ok(())
                };
                let _ = result.await;
            });
        }
        Ok::<(), anyhow::Error>(())
    });
    rt.shutdown_background();
    result
}
