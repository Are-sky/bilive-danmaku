
// #[allow(dead_code)]
use serde::{Deserialize};
use tokio_tungstenite as tokio_ws2;
use tokio_tungstenite::tungstenite as ws2;
use futures_util::{StreamExt, SinkExt};

use tokio::{sync::{mpsc, broadcast}, task::JoinHandle};

pub struct Uninited;

pub struct Disconnected {
    key: String,
    host_list: Vec<Host>,
}
pub struct Connected {
    pub fallback: Disconnected,
    broadcastor: broadcast::Sender<Event>,
    pub process_handle: JoinHandle<()>,
}

pub struct RoomService<S> {
    roomid: u64,
    status: S,
}

impl RoomService<()> {
    pub fn new(roomid: u64) -> RoomService<Uninited> {
        RoomService {
            roomid,
            status: Uninited{},
        }
    }
}

impl RoomService<Uninited> {
    pub async fn init(mut self) -> Result<RoomService<Disconnected>, (Self, ())> {
        let room_info_url = format!("https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id={}", self.roomid);
        match reqwest::get(room_info_url).await {
            Ok(resp) => {
                if resp.status().is_success() {
                    if let Ok(body) = resp.text().await {
                        let response_json_body:RoomPlayInfo = serde_json::from_str(body.as_str()).unwrap();
                        if let Some(data) = response_json_body.data {
                            self.roomid = data.room_id;
                        }
                    } else {
                        return Err((self, ()))
                    }
                } else {
                    return Err((self, ()))
                }
            }
            Err(_) => {
                return Err((self, ()))
            },
        }
        let url = format!("https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo?id={}&type=0", self.roomid);
        match reqwest::get(url).await {
            Ok(resp) => {
                if resp.status().is_success() {
                    if let Ok(body) = resp.text().await {
                        let response_json_body:Response = serde_json::from_str(body.as_str()).unwrap();
                        let status = Disconnected {
                            key: response_json_body.data.token,
                            host_list: response_json_body.data.host_list
                        };
                        Ok(RoomService {
                            roomid: self.roomid,
                            status
                        })
                    } else {
                        Err((self, ()))
                    }
                } else {
                    Err((self, ()))
                }
            }
            Err(_) => {
                Err((self, ()))
            },
        }
    }
}

impl RoomService<Disconnected> {
    pub async fn connect(self) -> Result<RoomService<Connected>, (Self, ConnectError)> {
        if self.status.host_list.is_empty() {
            return Err((self, ConnectError::HostListIsEmpty));
        }
        let url = self.status.host_list[0].wss();
        match tokio_ws2::connect_async(url).await {
            Ok((stream, _)) => {
                let auth = crate::Auth::new( 0, self.roomid, Some(self.status.key.clone()));
                let mut conn = RoomConnection::start(stream, auth).await.unwrap();
                let (broadcastor, _) = broadcast::channel::<Event>(128);
                let process_packet_broadcastor = broadcastor.clone();
                let process_packet = async move {
                    while let Some(packet) = conn.pack_rx.recv().await {
                        for data in packet.clone().get_datas() {
                            match data {
                                crate::Data::Json(json_val) => {
                                    match crate::cmd::Cmd::deser(json_val) {
                                        Ok(cmd) => {
                                            if let Some(evt) = cmd.as_event() {
                                                process_packet_broadcastor
                                                .send(evt)
                                                .unwrap_or_default();
                                            }
                                        }
                                        Err(_e) => {
                                            // println!("无法反序列化:\n{}", e);
                                        }
                                    }
                                },
                                crate::Data::Popularity(popularity) => {
                                    process_packet_broadcastor.send(
                                        Event::PopularityUpdate { popularity }
                                    ).unwrap_or_default();
                                },
                                crate::Data::Deflate(s) => {
                                    println!("deflate 压缩的消息（请报告此bug）: \n{}", s);
                                },
                            }
                        }
                    }
                };
                let process_handle = tokio::spawn(process_packet);
                let status = Connected {
                    fallback: self.status,
                    broadcastor,
                    process_handle,
                };
                Ok(RoomService {
                    roomid: self.roomid,
                    status
                })
            }
            Err(e) => {
                Err((self, ConnectError::WsError(e.to_string())))
            }
        }
    }
}

impl RoomService<Connected> {
    pub fn subscribe(&mut self) -> broadcast::Receiver<Event> {
        self.status.broadcastor.subscribe()
    }
}


#[derive(Debug, Deserialize)]
struct RoomPlayInfoData {
    room_id: u64,
}


/// 
/// api url:
/// https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id=510
#[derive(Debug, Deserialize)]
struct RoomPlayInfo {
    data: Option<RoomPlayInfoData>
}


#[derive(Debug, Deserialize)]
struct Response {
    // code: i32,
    // message: String,
    // ttl: i32,
    data: ResponseData
}
#[derive(Debug, Deserialize)]

struct ResponseData {
    // max_delay: i32,
    token: String,
    host_list: Vec<Host>
}

#[derive(Debug, Deserialize)]
struct Host {
    host: String,
    wss_port: u16,
}

impl Host {
    fn wss(&self) -> String {
        let host = &self.host;
        let port = self.wss_port;
        format!("wss://{host}:{port}/sub")
    }
}

#[derive(Debug)]
pub enum ConnectError {
    HostListIsEmpty,
    WsError(String),
}

use crate::{types::*, RawPacket, event::Event};
pub struct RoomConnection {
    pack_rx: mpsc::Receiver<RawPacket>,
}

impl RoomConnection {
    async fn start(ws_stream: WsStream, auth: crate::Auth) -> Result<Self, ()> {
        use ws2::Message::*;

        let (mut tx, mut rx) = ws_stream.split();
        let authpack_bin = RawPacket::build(crate::Operation::Auth, auth.ser()).ser();
        tx.send(Binary(authpack_bin)).await.unwrap();
        let _auth_reply = match rx.next().await {
            Some(Ok(Binary(auth_reply_bin))) => RawPacket::from_buffer(&auth_reply_bin),
            _ => return Err(()),
        };
        let channel_buffer_size = 64;
        let (pack_outbound_tx, mut pack_outbound_rx) = mpsc::channel::<RawPacket>(channel_buffer_size);
        let (pack_inbound_tx, pack_inbound_rx) = mpsc::channel::<RawPacket>(channel_buffer_size);

        let hb_sender = pack_outbound_tx.clone();

        let hb = async move {
            use tokio::time::{sleep, Duration};
            loop {
                hb_sender.send(RawPacket::heartbeat()).await.unwrap();
                sleep(Duration::from_secs(30)).await;
            }
        };

        let send = async move {
            while let Some(p) = pack_outbound_rx.recv().await {
                let bin= p.ser();
                tx.send(Binary(bin)).await.unwrap_or_default();
            }
        };

        let recv = async move {
            while let Some(Ok(msg)) = rx.next().await {
                match msg {
                    Binary(bin) => {                        
                        let packet = crate::RawPacket::from_buffer(&bin);
                        pack_inbound_tx.send(packet).await.unwrap_or_default();
                    },
                    Close(f) => {
                        println!("{:?}",f);
                    },
                    _ => {

                    }
                }
            }
        };

        tokio::spawn(send);
        tokio::spawn(recv);
        tokio::spawn(hb);

        Ok(RoomConnection{
            pack_rx: pack_inbound_rx
        })
    }

}
