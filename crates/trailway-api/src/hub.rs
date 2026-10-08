use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::mpsc;
use trailway_proto::ApiMessage;
use uuid::Uuid;

type Slot = (u64, mpsc::UnboundedSender<ApiMessage>);

/// The agents currently connected over the WebSocket, one per server.
#[derive(Clone, Default)]
pub struct Hub {
    inner: Arc<Mutex<HashMap<Uuid, Slot>>>,
    next: Arc<std::sync::atomic::AtomicU64>,
}

impl Hub {
    /// Registers a connection. A newer connection for the same server replaces
    /// the older one (its receiver closes, which ends that session).
    pub fn connect(&self, server_id: Uuid) -> (u64, mpsc::UnboundedReceiver<ApiMessage>) {
        let conn = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        self.inner.lock().unwrap().insert(server_id, (conn, tx));
        (conn, rx)
    }

    /// Removes the connection, unless a newer one already replaced it.
    pub fn disconnect(&self, server_id: Uuid, conn: u64) {
        let mut inner = self.inner.lock().unwrap();
        if inner.get(&server_id).is_some_and(|(c, _)| *c == conn) {
            inner.remove(&server_id);
        }
    }

    /// Queues a message for the server's agent. False when it is not connected.
    pub fn send(&self, server_id: Uuid, msg: ApiMessage) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(&server_id)
            .is_some_and(|(_, tx)| tx.send(msg).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn newest_connection_wins() {
        let hub = Hub::default();
        let id = Uuid::new_v4();
        let msg = ApiMessage::Stop {
            deployment_id: Uuid::nil(),
        };
        assert!(!hub.send(id, msg.clone()));
        let (c1, mut rx1) = hub.connect(id);
        let (c2, mut rx2) = hub.connect(id);
        assert!(hub.send(id, msg.clone()));
        assert_eq!(rx2.recv().await, Some(msg));
        assert_eq!(rx1.recv().await, None);
        hub.disconnect(id, c1);
        assert!(hub.send(id, ApiMessage::Stop { deployment_id: id }));
        hub.disconnect(id, c2);
        assert!(!hub.send(id, ApiMessage::Stop { deployment_id: id }));
    }
}
