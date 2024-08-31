use std::io::Write;

use easyp2p::p2p::{Receive, P2P};
use anyhow::Result;

fn read_line() -> String {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    line = line.trim().to_owned();

    line
}

#[tokio::main]
async fn main() -> Result<()> {
    print!("> ");
    std::io::stdout().flush().unwrap();
    let line = read_line();

    match &*line {
        "a" => client_a().await,
        _ => client_b(&line).await
    }
}

async fn client_a() -> Result<()> {
    let mut p2p = P2P::new(None).await?;

    let code = p2p.create_code("https://signaling.mii.dev").await?;

    println!("{code}");

    p2p.wait_open().await;

    println!("Connected!");

    chat(p2p).await
}

async fn client_b(code: &str) -> Result<()> {
    let mut p2p = P2P::new(None).await?;

    p2p.connect_with_code("https://signaling.mii.dev", code).await?;

    p2p.wait_open().await;

    println!("Connected!");

    chat(p2p).await
}

async fn chat(mut p2p: P2P) -> Result<()> {
    let receive_thread = tokio::spawn({
        let receive = p2p.receive_data_rx.clone();

        async move {
            let mut receive = receive.lock().await;
            while let Some(Receive::Data(data)) = receive.recv().await {
                print!("Received: {}", String::from_utf8(data.to_vec()).unwrap());
                std::io::stdout().flush().unwrap();
            }
            println!("P2P Closed!");
        }
    });

    let send_thread = tokio::spawn(async move {
        loop {
            let line = tokio::task::spawn_blocking(|| read_line()).await.unwrap();
            p2p.send_text(&format!("{line}\n")).await.unwrap();
        }
    });

    tokio::select! {
        _ = receive_thread => {},
        _ = send_thread => {}
    }

    Ok(())
}
