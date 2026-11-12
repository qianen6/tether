use crate::Server;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{collections::VecDeque, time::Duration};
use tether_proto::*;
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(super) enum Wire {
    Json(ServerMsg),
    Binary(Vec<u8>),
    Close(u16),
}
pub(super) struct Phone {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    pub inbox: VecDeque<Wire>,
    pub token: String,
    pub device: String,
    next_req: u64,
}
impl Phone {
    pub async fn connect(server: &Server) -> Self {
        let (socket, _) =
            tokio_tungstenite::connect_async(format!("ws://{}/v1/ws", server.local_addr()))
                .await
                .unwrap();
        Self {
            socket,
            inbox: VecDeque::new(),
            token: String::new(),
            device: String::new(),
            next_req: 0,
        }
    }
    pub async fn pair(server: &Server) -> Self {
        Self::pair_secret(server, server.new_pair_secret(Duration::from_secs(600))).await
    }
    pub async fn pair_secret(server: &Server, secret: String) -> Self {
        let mut phone = Self::connect(server).await;
        phone
            .send(json!({"t":"pair","version":1,"secret":secret,"device_name":"Pixel 测试"}))
            .await;
        match phone.read().await {
            Wire::Json(ServerMsg::Paired { token, device_id }) => {
                phone.token = token;
                phone.device = device_id;
            }
            other => panic!("expected paired, got {other:?}"),
        }
        phone.welcome().await;
        phone
    }
    pub async fn hello(server: &Server, token: &str) -> Self {
        let mut phone = Self::connect(server).await;
        phone
            .send(json!({"t":"hello","version":1,"token":token,"device_name":"renamed"}))
            .await;
        phone.token = token.into();
        phone.welcome().await;
        phone
    }
    async fn welcome(&mut self) {
        match self.read().await {
            Wire::Json(ServerMsg::Welcome {
                version,
                host_name,
                device_id,
                server_time,
            }) => {
                assert_eq!(version, 1);
                assert_eq!(host_name, "test-host");
                assert!(server_time > 0);
                self.device = device_id;
            }
            other => panic!("expected welcome, got {other:?}"),
        }
    }
    pub async fn send(&mut self, value: Value) {
        self.raw(Message::Text(value.to_string().into())).await;
    }
    pub async fn raw(&mut self, message: Message) {
        self.socket.send(message).await.unwrap();
    }
    pub async fn request(&mut self, t: &str, fields: Value) -> u64 {
        self.next_req += 1;
        let mut fields = fields.as_object().unwrap().clone();
        fields.insert("t".into(), t.into());
        fields.insert("req".into(), self.next_req.into());
        self.send(Value::Object(fields)).await;
        self.next_req
    }
    pub async fn ok(&mut self, req: u64) -> Value {
        match self.wait(|w| matches!(w, Wire::Json(ServerMsg::Ok { req: n, .. } | ServerMsg::Error { req: Some(n), .. }) if *n == req)).await {
            Wire::Json(ServerMsg::Ok { data, .. }) => data,
            other => panic!("request {req} failed: {other:?}"),
        }
    }
    pub async fn call(&mut self, t: &str, fields: Value) -> Value {
        let req = self.request(t, fields).await;
        self.ok(req).await
    }
    pub async fn error(&mut self, req: Option<u64>, code: ErrorCode) {
        self.wait(|w| matches!(w, Wire::Json(ServerMsg::Error { req: n, code: c, .. }) if *n == req && *c == code)).await;
    }
    pub async fn read(&mut self) -> Wire {
        loop {
            let frame = timeout(Duration::from_secs(5), self.socket.next())
                .await
                .expect("WebSocket receive timeout")
                .expect("WebSocket ended")
                .expect("WebSocket error");
            match frame {
                Message::Text(text) => {
                    return Wire::Json(serde_json::from_str(&text).expect("invalid ServerMsg"));
                }
                Message::Binary(bytes) => return Wire::Binary(bytes.to_vec()),
                Message::Close(frame) => {
                    return Wire::Close(frame.map(|f| u16::from(f.code)).unwrap_or(1000));
                }
                _ => {}
            }
        }
    }
    pub async fn wait(&mut self, predicate: impl Fn(&Wire) -> bool) -> Wire {
        if let Some(at) = self.inbox.iter().position(&predicate) {
            return self.inbox.remove(at).unwrap();
        }
        loop {
            let wire = self.read().await;
            if predicate(&wire) {
                return wire;
            }
            assert!(
                !matches!(wire, Wire::Close(_)),
                "unexpected socket close: {wire:?}"
            );
            self.inbox.push_back(wire);
        }
    }
    pub async fn close_code(&mut self, code: u16) {
        self.wait(|w| matches!(w, Wire::Close(c) if *c == code))
            .await;
    }
    pub async fn event(&mut self, session: &str, predicate: impl Fn(&EventKind) -> bool) -> Event {
        match self.wait(|w| matches!(w, Wire::Json(ServerMsg::Event { session: id, event }) if id == session && predicate(&event.kind))).await {
            Wire::Json(ServerMsg::Event { event, .. }) => event,
            _ => unreachable!(),
        }
    }
    pub async fn idle(&mut self, session: &str) -> Event {
        self.event(session, |e| {
            matches!(
                e,
                EventKind::State {
                    state: SessionState::Idle,
                    ..
                }
            )
        })
        .await
    }
    pub async fn create(&mut self) -> SessionInfo {
        serde_json::from_value(
            self.call("create_session", json!({"agent":"echo"})).await["session"].clone(),
        )
        .unwrap()
    }
    pub async fn subscribe(&mut self, id: &str, after: u64) {
        self.call("subscribe", json!({"session":id,"after":after}))
            .await;
    }
    pub async fn send_text(&mut self, id: &str, text: &str) {
        self.call("send_message", json!({"session":id,"text":text}))
            .await;
    }
    pub async fn term(&mut self) -> TermInfo {
        serde_json::from_value(
            self.call(
                "term_open",
                json!({"command":["fixture-shell"],"cols":80,"rows":24}),
            )
            .await["term"]
                .clone(),
        )
        .unwrap()
    }
    pub async fn bytes(&mut self, id: &str) -> Vec<u8> {
        match self.wait(|w| matches!(w, Wire::Binary(bytes) if binary::decode(bytes).is_ok_and(|(_, term, _)| term == id))).await {
            Wire::Binary(bytes) => { let (kind, term, data) = binary::decode(&bytes).unwrap(); assert_eq!(kind, binary::TERM_OUTPUT); assert_eq!(term, id); data.into() }
            _ => unreachable!(),
        }
    }
    pub async fn disconnect(mut self) {
        let _ = self.socket.close(None).await;
    }
}
