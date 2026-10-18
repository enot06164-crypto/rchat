//! Тесты самого testkit: транзакционность `MockStorage` и управляемые помехи
//! `MockTransport` на детерминированном стенде (без сети и сервера).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rchat_core::error::TransportError;
use rchat_core::traits::{Storage, Transport, TransportFactory};
use rchat_core::types::{MessageStatus, OutboxState, SyncApply};
use rchat_testkit::fixtures;
use rchat_testkit::{MockStorage, MockTransportFactory};

// ---------------------------------------------------------------------------
// MockStorage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mock_storage_batch_is_atomic_and_deduplicates() {
    let storage = MockStorage::new();
    let first = fixtures::incoming_message(1, "chat-1", "user-a", "первое");
    storage
        .apply_sync_batch(std::slice::from_ref(&SyncApply::InsertMessage(first)), 1)
        .await
        .expect("apply");
    assert_eq!(storage.get_sync_cursor().await.unwrap(), 1);
    assert!(storage.get_message("s:1").await.unwrap().is_some());

    // Инъекция сбоя: пачка не применяется, курсор не двигается.
    storage.fail_next_batch();
    let second = fixtures::incoming_message(2, "chat-1", "user-a", "второе");
    let result = storage
        .apply_sync_batch(&[SyncApply::InsertMessage(second)], 2)
        .await;
    assert!(result.is_err(), "ожидалась ошибка транзакции");
    assert_eq!(storage.get_sync_cursor().await.unwrap(), 1);
    assert!(storage.get_message("s:2").await.unwrap().is_none());

    // Повтор без сбоя проходит; дубликат по server_id с другим local_id
    // игнорируется, порядок — новые первыми.
    let third = fixtures::incoming_message(2, "chat-1", "user-a", "второе");
    storage
        .apply_sync_batch(&[SyncApply::InsertMessage(third)], 2)
        .await
        .expect("apply");
    let mut duplicate = fixtures::incoming_message(2, "chat-1", "user-a", "копия");
    duplicate.local_id = "local-x".to_string();
    storage
        .apply_sync_batch(&[SyncApply::InsertMessage(duplicate)], 2)
        .await
        .expect("apply");
    assert_eq!(storage.get_sync_cursor().await.unwrap(), 2);
    let messages = storage.list_messages("chat-1", None, 10).await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].server_id, Some(2));
    assert_eq!(messages[1].server_id, Some(1));
}

#[tokio::test]
async fn mock_storage_outbox_ack_removes_echo_copy() {
    let storage = MockStorage::new();
    let chat = fixtures::direct_chat("chat-1", "user-b");
    storage.upsert_chat(&chat, &[]).await.expect("chat");

    let (message, outbox) = fixtures::outgoing_message("local-1", "chat-1", "user-a", "привет");
    storage
        .insert_outgoing(&message, &outbox)
        .await
        .expect("outgoing");
    let due = storage.due_outbox(0, 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].state, OutboxState::Pending);

    // «Эхо» пришло событием раньше подтверждения: копия по server_id.
    let echo = fixtures::incoming_message(7, "chat-1", "user-a", "привет");
    storage
        .apply_sync_batch(&[SyncApply::InsertMessage(echo)], 1)
        .await
        .unwrap();

    storage.apply_send_ack("local-1", 7).await.expect("ack");
    let stored = storage.get_message("local-1").await.unwrap().unwrap();
    assert_eq!(stored.server_id, Some(7));
    assert_eq!(stored.status, Some(MessageStatus::Sent));
    assert!(
        storage.get_message("s:7").await.unwrap().is_none(),
        "эхо-копия удалена после подтверждения"
    );
    assert!(storage.get_outbox("local-1").await.unwrap().is_none());
    assert_eq!(
        storage
            .get_chat("chat-1")
            .await
            .unwrap()
            .unwrap()
            .last_server_id,
        7
    );
}

// ---------------------------------------------------------------------------
// MockTransport
// ---------------------------------------------------------------------------

/// Простейший транспорт стенда: кадры из очереди, пустая очередь — простой.
struct StubTransport {
    inbound: VecDeque<Vec<u8>>,
}

#[async_trait]
impl Transport for StubTransport {
    async fn send(&mut self, _frame: &[u8]) -> Result<(), TransportError> {
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        match self.inbound.pop_front() {
            Some(frame) => Ok(Some(frame)),
            None => std::future::pending().await,
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

struct StubFactory {
    frames: Mutex<Option<VecDeque<Vec<u8>>>>,
    connects: AtomicUsize,
}

#[async_trait]
impl TransportFactory for StubFactory {
    async fn connect(&self) -> Result<Box<dyn Transport>, TransportError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let frames = self.frames.lock().unwrap().take().unwrap_or_default();
        Ok(Box::new(StubTransport { inbound: frames }))
    }
}

#[tokio::test]
async fn mock_transport_injects_faults_in_order() {
    let stub = Arc::new(StubFactory {
        frames: Mutex::new(Some(VecDeque::from([b"one".to_vec(), b"two".to_vec()]))),
        connects: AtomicUsize::new(0),
    });
    let factory = MockTransportFactory::new(stub);
    let faults = factory.faults();
    let mut transport = factory.connect().await.expect("connect");
    assert_eq!(factory.connections(), 1);

    // Задержка + повтор: кадр приходит позже и дважды.
    faults.delay_next_inbound(Duration::from_millis(50));
    faults.duplicate_next_inbound();
    let started = Instant::now();
    assert_eq!(transport.recv().await.unwrap().unwrap(), b"one");
    assert!(started.elapsed() >= Duration::from_millis(40));
    assert_eq!(transport.recv().await.unwrap().unwrap(), b"one");

    // Порядок последующих кадров сохраняется.
    assert_eq!(transport.recv().await.unwrap().unwrap(), b"two");

    // Внедрённый кадр (битый/чужой) выдаётся до данных транспорта.
    faults.inject_inbound(vec![0x00, 0x01]);
    assert_eq!(transport.recv().await.unwrap().unwrap(), vec![0x00, 0x01]);

    // Разрыв и ошибки входящего направления.
    faults.drop_next_inbound();
    assert!(transport.recv().await.unwrap().is_none());
    faults.fail_next_inbound();
    assert!(transport.recv().await.is_err());

    // Исходящие: сбой, задержка, запись успешно отправленного.
    faults.fail_next_send();
    assert!(transport.send(b"x").await.is_err());
    faults.delay_next_send(Duration::from_millis(30));
    let started = Instant::now();
    transport.send(b"y").await.expect("send");
    assert!(started.elapsed() >= Duration::from_millis(20));
    assert_eq!(faults.sent_frames(), vec![b"y".to_vec()]);

    // Помехи переживают реконнект: очередь видна новому соединению.
    faults.inject_inbound(b"reconnected".to_vec());
    drop(transport);
    let mut second = factory.connect().await.expect("reconnect");
    assert_eq!(factory.connections(), 2);
    assert_eq!(second.recv().await.unwrap().unwrap(), b"reconnected");
}
