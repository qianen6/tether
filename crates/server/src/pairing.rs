//! Same failure-window policy as .ref/boop/src/pairing.ts (no code copied).
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{collections::HashMap, time::Duration};
use subtle::ConstantTimeEq;
use tether_proto::RejectReason;
use tokio::time::Instant;

#[derive(Default)]
pub(crate) struct Pairing {
    secrets: HashMap<[u8; 32], Instant>,
    failures: u8,
    locked_until: Option<Instant>,
}
impl Pairing {
    pub(crate) fn create(&mut self, ttl: Duration) -> String {
        let now = Instant::now();
        self.secrets.retain(|_, expires| *expires > now);
        let secret: [u8; 32] = rand::random();
        self.secrets
            .insert(secret, now + ttl.min(Duration::from_secs(15 * 60)));
        URL_SAFE_NO_PAD.encode(secret)
    }
    pub(crate) fn claim(&mut self, text: &str) -> std::result::Result<(), RejectReason> {
        let now = Instant::now();
        if self.locked_until.is_some_and(|t| t > now) {
            return Err(RejectReason::Locked);
        }
        self.secrets.retain(|_, expires| *expires > now);
        let decoded = URL_SAFE_NO_PAD
            .decode(text)
            .ok()
            .and_then(|s| <[u8; 32]>::try_from(s).ok());
        let found = decoded.and_then(|candidate| {
            self.secrets
                .keys()
                .find(|s| bool::from(s.ct_eq(&candidate)))
                .copied()
        });
        if let Some(secret) = found {
            self.secrets.remove(&secret);
            self.failures = 0;
            self.locked_until = None;
            return Ok(());
        }
        self.failures += 1;
        if self.failures >= 5 {
            self.locked_until = Some(now + Duration::from_secs(60));
            self.failures = 0;
        }
        Err(RejectReason::InvalidSecret)
    }
}
