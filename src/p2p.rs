use std::sync::Arc;

use crate::p2p_option::P2POption;
use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use  base64::{prelude::BASE64_STANDARD, Engine};
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc::{Receiver, Sender}, Mutex};
use webrtc::{api::{interceptor_registry::register_default_interceptors, media_engine::MediaEngine, APIBuilder}, data_channel::{data_channel_message::DataChannelMessage, RTCDataChannel}, ice_transport::{ice_candidate::RTCIceCandidate, ice_server::RTCIceServer}, interceptor::registry::Registry, peer_connection::{configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState, sdp::session_description::RTCSessionDescription, RTCPeerConnection}};

#[derive(Clone, Serialize, Deserialize)]
pub struct SessionDescription {
    pub session_description: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ConnectionCode {
    pub connection_code: String,
}

pub struct P2P {
    peer_connection: Arc<Mutex<RTCPeerConnection>>,
    send_data_tx: Arc<Mutex<Sender<SendData>>>,
    send_data_rx: Arc<Mutex<Receiver<SendData>>>,
    pub receive_data_rx: Arc<Mutex<Receiver<Receive>>>,
    receive_data_tx: Arc<Mutex<Sender<Receive>>>,
    done_tx: Arc<Mutex<Sender<()>>>,
    on_open: Receiver<()>,
    on_open_tx: Arc<Mutex<Sender<()>>>
}

enum SendData {
    Bytes(Bytes),
    String(String)
}

pub enum Receive {
    Data(Bytes),
    Close
}

impl P2P {
    pub async fn connect_with_code(&mut self, signaling_server: &str, code: &str) -> Result<()> {
        let client = Client::new();
        
        let res = client
            .get(format!("{}/session/{}", signaling_server, code))
            .send()
            .await?;

        let session: SessionDescription = if res.status().is_success() {
            res.json().await?
        } else {
            return Err(anyhow!("Failed to get remote session description"));
        };

        let answer = self.set_offer(&session.session_description, true).await?;

        let answer = SessionDescription {
            session_description: answer
        };
        let res = client
            .post(format!("{}/session/{}", signaling_server, code))
            .json(&answer)
            .send()
            .await?;

        if res.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!("Failed to send local session description"))
        }
    }

    pub async fn create_code(&mut self, signaling_server: &str) -> Result<String> {
        let client = Client::new();

        let description = SessionDescription {
            session_description: self.create_offer(true).await?,
        };
        
        let res = client
            .post(format!("{}/register", signaling_server))
            .json(&description)
            .send()
            .await?;

        let mut stream = res.bytes_stream();
        let (connection_code_tx, mut connection_code_rx) = tokio::sync::mpsc::channel::<String>(1);

        tokio::spawn({
            let peer_connection = self.peer_connection.clone();
            async move {
                let mut connection_code_received = false;
                'outer: while let Some(item) = stream.next().await {
                    let chunk = item.unwrap();
                    let data = String::from_utf8_lossy(&chunk);

                    for line in data.lines() {
                        if line.starts_with("data:") {
                            let value = line.trim_start_matches("data:").trim();
                            if connection_code_received {
                                peer_connection.lock().await
                                    .set_remote_description(
                                        Self::decode_description(value, true).unwrap()
                                    ).await.unwrap();
                                break 'outer;
                            } else {
                                connection_code_tx.send(value.to_string()).await.unwrap();
                                connection_code_received = true;
                            }
                        }
                    }
                }
            }
        });

        connection_code_rx.recv().await.context("Failed to get connection code")
    }

    pub async fn wait_open(&mut self) {
        self.on_open.recv().await;
    }

    pub async fn send(&mut self, data: Bytes) -> Result<()> {
        self.send_data_tx.lock().await.send(SendData::Bytes(data)).await.context("Failed to send data")
    }

    pub async fn send_text(&mut self, data: &str) -> Result<()> {
        self.send_data_tx.lock().await.send(SendData::String(data.to_string())).await.context("Failed to send data")
    }

    pub async fn receive(&mut self) -> Option<Receive> {
        self.receive_data_rx.lock().await.recv().await
    }

    pub async fn receive_text(&mut self) -> Result<String> {
        match self.receive().await.context("Failed to receive data")? {
            Receive::Data(data) => String::from_utf8(data.to_vec()).context("Failed to convert"),
            Receive::Close => Err(anyhow!("Session closed"))
        }
    }

    pub async fn set_answer(&mut self, answer: &str, compress: bool) -> Result<()> {
        self.peer_connection.lock().await.set_remote_description(Self::decode_description(answer, compress)?).await.context("Failed to set answer")
    }

    pub async fn set_offer(&mut self, offer: &str, compress: bool) -> Result<String> {
        let offer = Self::decode_description(offer, compress)?;

        let peer_connection = self.peer_connection.lock().await;

        let receive_data_tx = self.receive_data_tx.clone();
        let on_open_tx = self.on_open_tx.clone();
        let send_data_rx = self.send_data_rx.clone();
        let done_tx = self.done_tx.clone();
        peer_connection
            .on_data_channel(Box::new(move |d: Arc<RTCDataChannel>| {
                let receive_data_tx = receive_data_tx.clone();
                let on_open_tx = on_open_tx.clone();
                let send_data_rx = send_data_rx.clone();
                let done_tx = done_tx.clone();

                Box::pin(async move {
                    let d2 = Arc::clone(&d);

                    d.on_close(Box::new(move || {
                        let done_tx = done_tx.clone();

                        Box::pin(async move {
                            done_tx.lock().await.send(()).await.unwrap();
                        })
                    }));

                    d.on_message(Box::new(move |msg: DataChannelMessage| {
                        let receive_data_tx = receive_data_tx.clone();
            
                        Box::pin(async move {
                            receive_data_tx.lock().await.send(Receive::Data(msg.data)).await.unwrap();
                        })
                    }));

                    d.on_open(Box::new(move || {
                        let on_open_tx = on_open_tx.clone();
                        let send_data_rx = send_data_rx.clone();
                        Box::pin(async move {
                            on_open_tx.lock().await.send(()).await.unwrap();
                            while let Some(data) = send_data_rx.lock().await.recv().await {
                                match data {
                                    SendData::Bytes(bytes) => {
                                        d2.send(&bytes).await.unwrap();
                                    },
                                    SendData::String(string) => {
                                        d2.send_text(string).await.unwrap();
                                    }
                                }
                            }
                        })
                    }))
                })
            }));

        peer_connection.set_remote_description(offer).await?;

        let answer = peer_connection.create_answer(None).await?;

        let mut gather_complete = peer_connection.gathering_complete_promise().await;

        peer_connection.set_local_description(answer).await?;

        let _ = gather_complete.recv().await;

        Self::encode_description(&peer_connection.local_description().await.context("Failed to generate local_description.")?, compress)
    }

    pub async fn create_offer(&mut self, compress: bool) -> Result<String> {
        let peer_connection = Arc::clone(&self.peer_connection);
        let ice_candidates = Arc::new(Mutex::new(Vec::new()));
        let ice_candidates_clone = Arc::clone(&ice_candidates);
    
        peer_connection.lock().await.on_ice_candidate(Box::new(move |c: Option<RTCIceCandidate>| {
            let ice_candidates = Arc::clone(&ice_candidates_clone);
            Box::pin(async move {
                if let Some(candidate) = c {
                    ice_candidates.lock().await.push(candidate);
                }
            })
        }));

        let data_channel = self.peer_connection.lock().await.create_data_channel("data", None).await?;

        let receive_data_tx = self.receive_data_tx.clone();
        data_channel.on_message(Box::new(move |msg: DataChannelMessage| {
            let receive_data_tx = receive_data_tx.clone();

            Box::pin(async move {
                receive_data_tx.lock().await.send(Receive::Data(msg.data)).await.unwrap();
            })
        }));

        let done_tx = self.done_tx.clone();
        data_channel.on_close(Box::new(move || {
            let done_tx = done_tx.clone();

            Box::pin(async move {
                done_tx.lock().await.send(()).await.unwrap();
            })
        }));

        let d1 = Arc::clone(&data_channel);
        let on_open_tx = self.on_open_tx.clone();
        let send_data_rx = self.send_data_rx.clone();
        data_channel.on_open(Box::new(move || {
            let d2 = Arc::clone(&d1);
            let on_open_tx = on_open_tx.clone();
            let send_data_rx = send_data_rx.clone();
            Box::pin(async move {
                on_open_tx.lock().await.send(()).await.unwrap();
                while let Some(data) = send_data_rx.lock().await.recv().await {
                    match data {
                        SendData::Bytes(bytes) => {
                            d2.send(&bytes).await.unwrap();
                        },
                        SendData::String(string) => {
                            d2.send_text(string).await.unwrap();
                        }
                    }
                }
            })
        }));

        let offer = peer_connection.lock().await.create_offer(None).await?;
        
        peer_connection.lock().await.set_local_description(offer).await?;

        let mut gather_complete = peer_connection.lock().await.gathering_complete_promise().await;
        let _ = gather_complete.recv().await;

        let local_desc = peer_connection.lock().await.local_description().await.context("Failed to generate local_description.")?;
        let mut sdp = local_desc.sdp.clone();

        sdp = sdp.lines()
            .filter(|line| !line.starts_with("a=candidate:udp"))
            .collect::<Vec<&str>>()
            .join("\r\n");

        Self::encode_description(&RTCSessionDescription::offer(sdp)?, compress)
    }

    fn encode_description(description: &RTCSessionDescription, compress: bool) -> Result<String> {
        let json_str = serde_json::to_string(description)?;
        if compress {
            let compressed = zstd::encode_all(json_str.as_bytes(), 3).unwrap();
            Ok(BASE64_STANDARD.encode(compressed))
        } else {
            Ok(BASE64_STANDARD.encode(&json_str))
        }
    }

    fn decode_description(description: &str, compress: bool) -> Result<RTCSessionDescription> {
        let description = if compress {
            String::from_utf8(zstd::decode_all(BASE64_STANDARD.decode(description)?.as_slice())?)?
        } else {
            String::from_utf8(BASE64_STANDARD.decode(description)?)?
        };

        serde_json::from_str(&description).context("Failed to parse description")
    }

    pub async fn new(option: Option<P2POption>) -> Result<Self> {
        let mut m = MediaEngine::default();
        let _ = m.register_default_codecs();

        let mut registry = Registry::new();

        registry = register_default_interceptors(registry, &mut m)?;

        let api = APIBuilder::new()
            .with_media_engine(m)
            .with_interceptor_registry(registry)
            .build();

        let config = if let Some(option) = option {
            option
        } else {
            P2POption {
                rtc_configuration: RTCConfiguration {
                    ice_servers: vec![
                        RTCIceServer {
                            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                            ..Default::default()
                        }
                    ],
                    ..Default::default()
                }
            }
        };

        let peer_connection = Arc::new(Mutex::new(api.new_peer_connection(config.rtc_configuration).await?));

        let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<()>(1);

        let done_tx = Arc::new(Mutex::new(done_tx));
        peer_connection.lock().await.on_peer_connection_state_change(Box::new({
            let done_tx = done_tx.clone();
            move |s: RTCPeerConnectionState| {
                let done_tx = done_tx.clone();
                Box::pin(async move {
                    if s == RTCPeerConnectionState::Failed || s == RTCPeerConnectionState::Disconnected || s == RTCPeerConnectionState::Closed {
                        let _ = done_tx.lock().await.try_send(());
                    }
                })
            }
        }));

        let (send_data_tx, send_data_rx) = tokio::sync::mpsc::channel::<SendData>(128);
        let (on_open_tx, on_open_rx) = tokio::sync::mpsc::channel::<()>(1);
        let (receive_data_tx, receive_data_rx) = tokio::sync::mpsc::channel::<Receive>(128);

        let send_data_tx = Arc::new(Mutex::new(send_data_tx));
        let send_data_rx = Arc::new(Mutex::new(send_data_rx));
        let receive_data_rx =  Arc::new(Mutex::new(receive_data_rx));
        let receive_data_tx = Arc::new(Mutex::new(receive_data_tx)); 

        tokio::spawn({
            let peer_connection = peer_connection.clone();
            let receive_data_tx = receive_data_tx.clone();
            async move {
                if let Some(()) = done_rx.recv().await {
                    peer_connection.lock().await.close().await.unwrap();
                    receive_data_tx.lock().await.send(Receive::Close).await.unwrap();
                    receive_data_tx.lock().await.closed().await;
                }
            }
        });

        Ok(
            Self {
                peer_connection,
                send_data_tx,
                send_data_rx,
                receive_data_rx,
                receive_data_tx,
                done_tx,
                on_open: on_open_rx,
                on_open_tx: Arc::new(Mutex::new(on_open_tx))
            }
        )
    }
}
