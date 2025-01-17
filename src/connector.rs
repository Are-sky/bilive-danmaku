use serde::Deserialize;

use crate::{connection::*, packet::*};

#[derive(Debug, Clone)]
pub struct Connector {
    pub roomid: u64,
    pub uid: u64,
    pub token: String,
    pub host_index: usize,
    pub host_list: Vec<Host>,
}

#[derive(Debug)]
pub enum InitError {
    ParseError(String),
    HttpError(reqwest::Error),
    DeserError(serde_json::Error),
}

impl From<serde_json::Error> for InitError {
    fn from(val: serde_json::Error) -> Self {
        InitError::DeserError(val)
    }
}

impl From<reqwest::Error> for InitError {
    fn from(err: reqwest::Error) -> Self {
        InitError::HttpError(err)
    }
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitError::ParseError(msg) => write!(f, "ParseError: {}", msg),
            InitError::HttpError(err) => write!(f, "HttpError: {}", err),
            InitError::DeserError(err) => write!(f, "DeserError: {}", err),
        }
    }
}

impl Connector {
    pub async fn init(mut roomid: u64) -> Result<Self, InitError> {
        let client = reqwest::Client::new();
        let room_info_url = format!(
            "https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id={}",
            roomid
        );
        let RoomPlayInfoData {
            room_id: real_room_id,
            uid,
        } = client
            .get(room_info_url)
            .send()
            .await?
            .json::<RoomPlayInfo>()
            .await?
            .data
            .ok_or(InitError::ParseError(format!("Fail to get room info")))?;
        roomid = real_room_id;
        let url = format!(
            "https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo?id={}&type=0",
            roomid
        );
        let DanmuInfoData { token, host_list } = client
            .get(url)
            .send()
            .await?
            .json::<DanmuInfo>()
            .await?
            .data
            .ok_or(InitError::ParseError(format!("Fail to get danmu info")))?;
        let connector = Connector {
            uid,
            host_index: 0,
            roomid,
            token,
            host_list,
        };
        Ok(connector)
    }

    pub fn use_host(&mut self, index: usize) -> Result<&'_ str, usize> {
        if self.host_list.len() > index {
            self.host_index = index;
            Ok(&self.host_list[index].host)
        } else {
            Err(self.host_list.len())
        }
    }

    pub async fn connect(&self) -> Result<Connection, ConnectError> {
        if self.host_list.is_empty() {
            return Err(ConnectError::HostListIsEmpty);
        }
        let url = self.host_list[self.host_index].wss();
        let roomid = self.roomid;
        let backup = self.clone();
        let auth = Auth::new(self.uid, roomid, Some(backup.token.clone()));
        let stream = Connection::connect(url, auth).await.map_err(|e| {
            log::error!("handshake error: {:?}", e);
            ConnectError::HandshakeError
        })?;
        Ok(stream)
    }
}

#[derive(Debug, Deserialize)]
struct RoomPlayInfoData {
    room_id: u64,
    uid: u64,
}

///
/// api url:
/// https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id=510
#[derive(Debug, Deserialize)]
struct RoomPlayInfo {
    data: Option<RoomPlayInfoData>,
}

#[derive(Debug, Deserialize)]
struct DanmuInfo {
    // code: i32,
    // message: String,
    // ttl: i32,
    data: Option<DanmuInfoData>,
}
#[derive(Debug, Deserialize)]

struct DanmuInfoData {
    // max_delay: i32,
    token: String,
    host_list: Vec<Host>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Host {
    pub host: String,
    pub wss_port: u16,
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
    HandshakeError,
    WsError(String),
}
