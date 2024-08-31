use std::io::Write;

use easyp2p::p2p::P2P;
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
    tokio::spawn({
        let receive = p2p.receive_data.clone();

        async move {
            let mut receive = receive.lock().await;
            while let Some(data) = receive.recv().await {
                print!("Received: {}\n", String::from_utf8(data.to_vec()).unwrap());
                std::io::stdout().flush().unwrap();
            }
        }
    });

    loop {
        let line = read_line();
        p2p.send_text(&format!("{line}\n")).await?;
    }
}