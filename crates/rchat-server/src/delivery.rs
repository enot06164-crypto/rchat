//! Реестр активных соединений и best-effort доставка push-событий.
//!
//! Реестр — в памяти процесса: это осознанное решение MVP для одного
//! экземпляра сервера (см. `docs/architecture.md`). Устойчивое состояние
//! (журнал синхронизации) живёт в `Repo`; push — только оптимизация
//! задержки, клиент всегда может догрузить события по курсору.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::mpsc;

/// Идентификатор соединения (уникален в рамках процесса).
pub type ConnId = u64;

/// Канал push-кадров (уже сериализованные envelope-ы для этого соединения).
pub type PushSender = mpsc::Sender<Vec<u8>>;

struct ConnEntry {
    device_id: String,
    tx: PushSender,
}

#[derive(Default)]
struct State {
    next_id: ConnId,
    by_user: HashMap<String, HashMap<ConnId, ConnEntry>>,
}

/// Реестр соединений. Потокобезопасен; сами каналы — tokio mpsc.
pub struct Connections {
    inner: Mutex<State>,
}

impl Default for Connections {
    fn default() -> Self {
        Self::new()
    }
}

impl Connections {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(State::default()),
        }
    }

    /// Регистрирует соединение и возвращает идентификатор и приёмник push.
    /// `buffer` — размер очереди push-кадров; при переполнении соединение
    /// считается не успевающим и закрывается (клиент догрузит по курсору).
    pub fn register(
        &self,
        user_id: &str,
        device_id: &str,
        buffer: usize,
    ) -> (ConnId, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel(buffer.max(1));
        let mut inner = self.inner.lock().expect("connections poisoned");
        inner.next_id += 1;
        let conn_id = inner.next_id;
        inner
            .by_user
            .entry(user_id.to_string())
            .or_default()
            .insert(
                conn_id,
                ConnEntry {
                    device_id: device_id.to_string(),
                    tx,
                },
            );
        (conn_id, rx)
    }

    /// Убирает соединение; возвращает (user_id, device_id), если оно было.
    pub fn unregister(&self, conn_id: ConnId) -> Option<(String, String)> {
        let mut inner = self.inner.lock().expect("connections poisoned");
        let user_id = inner
            .by_user
            .iter()
            .find(|(_, conns)| conns.contains_key(&conn_id))
            .map(|(user, _)| user.clone())?;
        let conns = inner.by_user.get_mut(&user_id)?;
        let entry = conns.remove(&conn_id)?;
        let device_id = entry.device_id.clone();
        if conns.is_empty() {
            inner.by_user.remove(&user_id);
        }
        Some((user_id, device_id))
    }

    pub fn is_online(&self, user_id: &str) -> bool {
        let inner = self.inner.lock().expect("connections poisoned");
        inner
            .by_user
            .get(user_id)
            .map(|conns| !conns.is_empty())
            .unwrap_or(false)
    }

    pub fn online_users(&self) -> Vec<String> {
        let inner = self.inner.lock().expect("connections poisoned");
        inner.by_user.keys().cloned().collect()
    }

    /// Устройства пользователя с активными соединениями.
    pub fn connected_devices(&self, user_id: &str) -> Vec<String> {
        let inner = self.inner.lock().expect("connections poisoned");
        let mut devices: Vec<String> = inner
            .by_user
            .get(user_id)
            .map(|conns| conns.values().map(|e| e.device_id.clone()).collect())
            .unwrap_or_default();
        devices.sort();
        devices.dedup();
        devices
    }

    /// Best-effort доставка кадра конкретному устройству пользователя.
    /// Неуспешные/переполненные каналы удаляются.
    pub fn deliver_device(&self, user_id: &str, device_id: &str, frame: Vec<u8>) -> bool {
        let mut inner = self.inner.lock().expect("connections poisoned");
        let Some(conns) = inner.by_user.get_mut(user_id) else {
            return false;
        };
        let mut delivered = false;
        let mut dead: Vec<ConnId> = Vec::new();
        for (conn_id, entry) in conns.iter() {
            if entry.device_id != device_id {
                continue;
            }
            match entry.tx.try_send(frame.clone()) {
                Ok(()) => delivered = true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!(conn_id = *conn_id, "push queue full; dropping connection");
                    dead.push(*conn_id);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    dead.push(*conn_id);
                }
            }
        }
        for conn_id in dead {
            conns.remove(&conn_id);
        }
        if conns.is_empty() {
            inner.by_user.remove(user_id);
        }
        delivered
    }

    /// Принудительно закрывает соединения конкретного устройства
    /// (например, после отзыва устройства). Возвращает число закрытых.
    pub fn disconnect_device(&self, user_id: &str, device_id: &str) -> usize {
        let mut inner = self.inner.lock().expect("connections poisoned");
        let Some(conns) = inner.by_user.get_mut(user_id) else {
            return 0;
        };
        let victims: Vec<ConnId> = conns
            .iter()
            .filter(|(_, e)| e.device_id == device_id)
            .map(|(id, _)| *id)
            .collect();
        for conn_id in &victims {
            conns.remove(conn_id);
        }
        let count = victims.len();
        if conns.is_empty() {
            inner.by_user.remove(user_id);
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_deliver_unregister() {
        let conns = Connections::new();
        let (id, mut rx) = conns.register("u1", "d1", 4);
        assert!(conns.is_online("u1"));
        assert_eq!(conns.connected_devices("u1"), vec!["d1".to_string()]);
        assert!(conns.deliver_device("u1", "d1", vec![1, 2, 3]));
        assert_eq!(rx.try_recv().unwrap(), vec![1, 2, 3]);
        assert!(!conns.deliver_device("u1", "d2", vec![9]));
        assert_eq!(conns.unregister(id), Some(("u1".into(), "d1".into())));
        assert!(!conns.is_online("u1"));
    }

    #[test]
    fn full_queue_drops_connection() {
        let conns = Connections::new();
        let (_id, _rx) = conns.register("u1", "d1", 1);
        assert!(conns.deliver_device("u1", "d1", vec![1]));
        assert!(!conns.deliver_device("u1", "d1", vec![2]));
        assert!(!conns.is_online("u1"));
    }

    #[test]
    fn disconnect_device_kicks() {
        let conns = Connections::new();
        let (_id, _rx) = conns.register("u1", "d1", 2);
        let (_id2, _rx2) = conns.register("u1", "d2", 2);
        assert_eq!(conns.disconnect_device("u1", "d1"), 1);
        assert_eq!(conns.connected_devices("u1"), vec!["d2".to_string()]);
    }
}
