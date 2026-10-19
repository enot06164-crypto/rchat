//! Сквозной сценарий «мок-стек»: настоящий сервер по каналам в памяти,
//! клиентское ядро с in-memory хранилищем из testkit и транспортом с
//! управляемыми помехами (разрыв, задержка, повтор кадра, битые кадры).
//!
//! Проверяются инварианты спецификации: outbox не теряет сообщение при
//! сбоях и переподключении, подтверждения идемпотентны, синхронизация
//! устойчива к повторной и повреждённой доставке.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rchat_core::client::{Client, ClientBuilder};
use rchat_core::config::ClientConfig;
use rchat_core::error::CoreError;
use rchat_core::traits::Storage;
use rchat_core::types::ChatMode;
use rchat_core::types::{ConnectionState, MessageRecord, MessageStatus};
use rchat_testkit::mock_storage::{MockSecretStore, MockStorage};
use rchat_testkit::mock_transport::MockTransportFactory;
use rchat_testkit::server::TestServer;
use rchat_testkit::FaultHandle;

const TIMEOUT: Duration = Duration::from_secs(20);

/// Клиент на mock-компонентах: канал в память, in-memory хранилище и секреты.
struct MockClient {
    client: Client,
    storage: Arc<MockStorage>,
    transport: Arc<MockTransportFactory>,
}

impl MockClient {
    fn faults(&self) -> FaultHandle {
        self.transport.faults()
    }
}

async fn start_client(server: &TestServer, device_name: &str) -> MockClient {
    let transport = Arc::new(MockTransportFactory::new(server.channel_factory()));
    let storage = Arc::new(MockStorage::new());
    let secrets = Arc::new(MockSecretStore::new());
    let config = ClientConfig {
        device_name: device_name.to_string(),
        client_name: "rchat-mock-stack".to_string(),
        e2e_enabled: false,
        ..Default::default()
    };
    let client = ClientBuilder::new(config, transport.clone(), storage.clone(), secrets)
        .build()
        .expect("client build");
    client.start().await.expect("client start");
    MockClient {
        client,
        storage,
        transport,
    }
}

/// Регистрация с повтором на редкое окно переподключения.
async fn register(client: &Client, username: &str, password: &str) {
    for attempt in 1..=5 {
        match client.register(username, username, password).await {
            Ok(()) => return,
            Err(CoreError::NotConnected) if attempt < 5 => {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            Err(error) => panic!("register {username}: {error}"),
        }
    }
    panic!("register {username}: исчерпаны попытки");
}

async fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !check() {
        assert!(Instant::now() < deadline, "не дождались: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_sent(storage: &MockStorage, local_id: &str) -> MessageRecord {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(message) = storage.get_message(local_id).await.expect("get message") {
            match message.status {
                Some(MessageStatus::Sent) => return message,
                Some(MessageStatus::Failed) => panic!("отправка {local_id} завершилась ошибкой"),
                _ => {}
            }
        }
        assert!(
            Instant::now() < deadline,
            "не дождались подтверждения отправки {local_id}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_body(client: &Client, chat_id: &str, needle: &str) -> MessageRecord {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let messages = client
            .list_messages(chat_id, None, 50)
            .await
            .expect("list messages");
        if let Some(message) = messages
            .into_iter()
            .find(|m| m.body.as_deref() == Some(needle))
        {
            return message;
        }
        assert!(
            Instant::now() < deadline,
            "не дождались сообщения {needle:?}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn wait_for_chat(storage: &MockStorage, chat_id: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while storage.get_chat(chat_id).await.expect("get chat").is_none() {
        assert!(Instant::now() < deadline, "не дождались чата {chat_id}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Обрыв соединения, реконнект и отправка на нестабильной связи: сообщение
/// уходит ровно один раз, outbox очищается, получатель видит одну копию.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_survives_faults_and_reconnect() {
    let server = TestServer::start_in_memory(|_| {}).await;
    let alice = start_client(&server, "alice-dev").await;
    let bob = start_client(&server, "bob-dev").await;
    register(&alice.client, "alice", "pw-alice-1").await;
    register(&bob.client, "bob", "pw-bob-1").await;

    let chat = alice
        .client
        .create_direct_chat("bob", ChatMode::NoneDev)
        .await
        .expect("create chat");
    wait_for_chat(&bob.storage, &chat.chat_id).await;

    // Разрыв: сервер «закрывает» соединение; клиент обязан вернуться сам.
    alice.faults().drop_next_inbound();
    wait_until("реконнект alice", || {
        alice.transport.connections() >= 2
    })
    .await;

    // Отправка на фоне задержки и повтора входящих кадров.
    alice
        .faults()
        .delay_next_inbound(Duration::from_millis(150));
    alice.faults().duplicate_next_inbound();
    let local_id = alice
        .client
        .send_text(&chat.chat_id, "привет из мока", None)
        .await
        .expect("send");

    let sent = wait_sent(&alice.storage, &local_id).await;
    assert!(
        sent.server_id.is_some(),
        "у отправленного сообщения должен быть server_id"
    );
    assert!(
        alice
            .storage
            .get_outbox(&local_id)
            .await
            .expect("outbox")
            .is_none(),
        "после подтверждения запись outbox удаляется"
    );

    // Получатель видит ровно одну копию; дубликаты не размножаются.
    let received = wait_for_body(&bob.client, &chat.chat_id, "привет из мока").await;
    assert_eq!(received.sender_user_id, sent.sender_user_id);
    tokio::time::sleep(Duration::from_millis(300)).await;
    for (name, client) in [("alice", &alice.client), ("bob", &bob.client)] {
        let messages = client
            .list_messages(&chat.chat_id, None, 50)
            .await
            .expect("messages");
        let copies = messages
            .iter()
            .filter(|m| m.body.as_deref() == Some("привет из мока"))
            .count();
        assert_eq!(copies, 1, "у {name} должна быть одна копия сообщения");
    }
    server.shutdown().await;
}

/// Повтор и порча кадров: одиночный битый кадр пропускается, серия (>10)
/// закрывает соединение, после чего клиент переподключается и синхронизируется.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_frames_and_malformed_input_survive() {
    let server = TestServer::start_in_memory(|_| {}).await;
    let alice = start_client(&server, "alice-dev").await;
    let bob = start_client(&server, "bob-dev").await;
    register(&alice.client, "alice", "pw-alice-1").await;
    register(&bob.client, "bob", "pw-bob-1").await;

    let chat = alice
        .client
        .create_direct_chat("bob", ChatMode::NoneDev)
        .await
        .expect("create chat");
    wait_for_chat(&bob.storage, &chat.chat_id).await;

    // Повтор кадра: следующий входящий кадр (push о сообщении) придёт дважды.
    alice.faults().duplicate_next_inbound();
    bob.client
        .send_text(&chat.chat_id, "первое", None)
        .await
        .expect("bob send");
    wait_for_body(&alice.client, &chat.chat_id, "первое").await;
    assert_eq!(
        alice.transport.connections(),
        1,
        "повтор кадра не должен рвать соединение"
    );

    // Одиночный битый кадр: безопасно пропускается, соединение живо.
    alice.faults().inject_inbound(vec![0x00, 0x01, 0x02]);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(alice.transport.connections(), 1);
    alice
        .client
        .sync_now()
        .await
        .expect("sync после битого кадра");

    // Серия битых кадров превышает порог терпимости (11-й кадр в рамках
    // одного соединения): соединение закрывается, клиент переподключается
    // и продолжает работу. Одиночный кадр выше уже израсходовал квоту — до
    // порога ровно один кадр, поэтому очередь помех опустошается точно.
    for _ in 0..10 {
        alice.faults().inject_inbound(vec![0x00, 0x01, 0x02]);
    }
    wait_until(
        "реконнект после серии битых кадров",
        || alice.transport.connections() >= 2,
    )
    .await;
    alice
        .client
        .sync_now()
        .await
        .expect("sync после реконнекта");
    let messages = alice
        .client
        .list_messages(&chat.chat_id, None, 50)
        .await
        .expect("messages");
    assert!(
        messages.iter().any(|m| m.body.as_deref() == Some("первое")),
        "после реконнекта клиент сохраняет полученные сообщения"
    );
    server.shutdown().await;
}

/// Регрессия: состояние соединения обязано сохраняться, даже если клиент
/// успел подключиться до появления первого подписчика (`wait_online`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_state_survives_absence_of_observers() {
    let server = TestServer::start_in_memory(|_| {}).await;
    let alice = start_client(&server, "alice-dev").await;

    // Никаких подписок: ждём сам переход состояния через явный опрос.
    wait_until(
        "соединение без наблюдателей",
        || alice.client.connection_state() == ConnectionState::Unauthenticated,
    )
    .await;

    // Регистрация проходит сразу: наблюдать «отключение» клиент не должен.
    alice
        .client
        .register("alice", "Alice", "pw-alice-1")
        .await
        .expect("register без наблюдателей");
    server.shutdown().await;
}
