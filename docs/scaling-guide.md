# Руководство по масштабированию rchat

Этот документ описывает путь от однопроцессного сервера (текущая реализация) к
горизонтально масштабируемому кластеру. Он предназначен для случая, когда
производительности одного процесса становится недостаточно.

## Содержание

1. [Текущая архитектура и её ограничения](#1-текущая-архитектура-и-её-ограничения)
2. [Когда масштабировать](#2-когда-масштабировать)
3. [Анализ узких мест](#3-анализ-узких-мест)
4. [Целевая архитектура кластера](#4-целевая-архитектура-кластера)
5. [Выбор компонентов](#5-выбор-компонентов)
6. [План миграции (7 этапов)](#6-план-миграции-7-этапов)
7. [Оценка трудозатрат](#7-оценка-трудозатрат)
8. [Альтернативные стратегии](#8-альтернативные-стратегии)
9. [Чеклист готовности](#9-чеклист-готовности)

---

## 1. Текущая архитектура и её ограничения

### Что хранится в памяти процесса

```
┌─────────────────────────────────────┐
│     rchat-server (один процесс)     │
├─────────────────────────────────────┤
│  Реестр соединений (Connections)    │ ← HashMap<user_id, devices>
│  Presence (online/offline)          │ ← в памяти, теряется при рестарте
│  Реестр звонков (CallRegistry)      │ ← активные звонки
│  Rate-limiting счётчики             │ ← память, сбрасывается при рестарте
├─────────────────────────────────────┤
│  SQLite (локальный файл)            │ ← аккаунты, сообщения, журнал sync
│  Блобы файлов (локальный каталог)  │ ← содержимое вложений
└─────────────────────────────────────┘
```

### Ограничения однопроцессной модели

| Аспект | Ограничение | Последствия |
|--------|-------------|-------------|
| **Соединения** | ~10,000–50,000 WebSocket на процесс | Упирается в дескрипторы и память |
| **CPU** | Один процесс = один core для sync-операций | SQLite блокирует при записи |
| **Доступность** | Перезапуск = отключение всех клиентов | Downtime при деплое |
| **География** | Один дата-центр | Высокая задержка для удалённых пользователей |
| **Presence** | Теряется при рестарте | Все считаются офлайн после обновления |
| **Звонки** | Теряются при рестарте | Активные звонки обрываются |

### Преимущества, которые потеряем

- ✅ Простота: `cargo run --bin rchat-server`
- ✅ Нет внешних зависимостей (PostgreSQL, Redis, Kafka)
- ✅ Локальная разработка без Docker
- ✅ SQLite: атомарные транзакции, встроенный, быстрый для чтения
- ✅ Простая отладка: весь стек в одном процессе

---

## 2. Когда масштабировать

### Технические индикаторы (измеримые)

**Немедленно масштабировать, если:**
1. **CPU utilization > 80%** на пиковой нагрузке (команда `top`, метрики Prometheus)
2. **Активных WebSocket > 10,000** одновременно
3. **SQLite блокируется** дольше 100ms (PRAGMA busy_timeout показывает конфликты)
4. **Память > 4 ГБ** на реестр соединений
5. **Latency p99 > 500ms** для отправки сообщения

**Начать планировать, если:**
1. Активных пользователей > 5,000 (подход к пределу)
2. Сообщений в сутки > 500,000
3. Файлов на диске > 100 ГБ (нужна распределённая файловая система)
4. Запланирован выход на международный рынок (нужны региональные серверы)

### Бизнес-индикаторы

1. **SLA требует 99.9%** uptime (≤ 8.76 часов downtime/год)
2. **Платящие пользователи** жалуются на недоступность
3. **Инвесторы требуют** готовность к 100x росту
4. **Конкуренты предлагают** multi-region deployment

### Антипаттерн: преждевременная оптимизация

❌ **Не масштабируйте, если:**
- Пользователей < 1,000
- Жалоб на производительность нет
- CPU < 50%, память < 2 ГБ
- Нет команды для поддержки инфраструктуры (кластер требует DevOps)

> **Правило:** масштабируйте, когда **измерения показывают проблему**, а не
> когда «вдруг понадобится».

---

## 3. Анализ узких мест

### Профилирование текущей системы

#### 3.1. Метрики для сбора

Добавьте в сервер перед миграцией:

```rust
// crates/rchat-server/src/metrics.rs (новый модуль)
use prometheus::{IntGauge, Histogram, register_int_gauge, register_histogram};

lazy_static! {
    pub static ref ACTIVE_CONNECTIONS: IntGauge = 
        register_int_gauge!("rchat_active_connections", "Active WebSocket connections").unwrap();
    
    pub static ref MESSAGE_LATENCY: Histogram = 
        register_histogram!("rchat_message_latency_seconds", "Message send latency").unwrap();
    
    pub static ref SQLITE_QUERY_DURATION: Histogram = 
        register_histogram!("rchat_sqlite_query_seconds", "SQLite query duration").unwrap();
}
```

#### 3.2. Точки измерения

| Метрика | Где измерять | Критическое значение |
|---------|--------------|----------------------|
| Активных соединений | `Connections::register/unregister` | > 10,000 |
| Latency отправки | `send_message` end-to-end | p99 > 500ms |
| SQLite read latency | `Repo::get_history` | p99 > 50ms |
| SQLite write latency | `Repo::insert_message` | p99 > 100ms |
| Размер `sync_events` | `SELECT COUNT(*) FROM sync_events` | > 1M строк |
| CPU per core | `tokio-metrics` или `top` | > 80% на любом ядре |

#### 3.3. Типичные узкие места

**Проблема 1: SQLite блокируется при записи**
```sql
-- Проверка: сколько времени ждём блокировки
PRAGMA busy_timeout;  -- если > 1000ms, есть проблема
```

**Решение:** PostgreSQL с connection pool (читай и пиши параллельно).

**Проблема 2: Реестр соединений занимает много памяти**
```rust
// Connections в памяти: ~1 КБ на соединение
// 50,000 соединений = 50 МБ + overhead структур = ~200 МБ
```

**Решение:** Redis Cluster для распределённого реестра.

**Проблема 3: Один процесс обрабатывает всё**
```
Все операции (auth, send, sync, presence) → один event loop tokio
```

**Решение:** Несколько серверов + load balancer.

---

## 4. Целевая архитектура кластера

### Диаграмма целевой системы

```
                        Интернет
                           │
                           ▼
                 ┌─────────────────┐
                 │  Load Balancer  │  ← HAProxy / nginx (sticky sessions)
                 │   (HAProxy)     │
                 └────────┬────────┘
                          │
         ┌────────────────┼────────────────┐
         ▼                ▼                ▼
    ┌─────────┐      ┌─────────┐      ┌─────────┐
    │ Server1 │      │ Server2 │      │ Server3 │  ← rchat-server (gateway)
    └────┬────┘      └────┬────┘      └────┬────┘
         │                │                │
         └────────────────┼────────────────┘
                          ▼
         ┌────────────────────────────────┐
         │     Redis Cluster (3 nodes)    │  ← Реестр соединений, presence,
         │  ┌──────┬──────┬──────┐        │     rate-limiting
         │  │Node1 │Node2 │Node3 │        │
         └──┴──────┴──────┴──────┴────────┘
                          │
         ┌────────────────┼────────────────┐
         ▼                ▼                ▼
    ┌──────────────────────────────────────┐
    │   PostgreSQL Primary + 2 Replicas    │  ← Сообщения, аккаунты, журнал
    │  (read replicas для истории)         │
    └──────────────────────────────────────┘
                          │
                          ▼
         ┌────────────────────────────────┐
         │      Message Bus (NATS)        │  ← Межсерверная доставка событий
         │  (альтернатива: Kafka/Redis)   │
         └────────────────────────────────┘
                          │
                          ▼
         ┌────────────────────────────────┐
         │   Object Storage (S3/MinIO)    │  ← Блобы файлов (вложения)
         └────────────────────────────────┘
```

### Поток отправки сообщения в кластере

```
1. Client → Server1: send_message(chat_id, text)
2. Server1 → PostgreSQL: INSERT message + idempotency_key
3. Server1 → Redis: GET devices_of(recipients)
4. Server1 → NATS: PUBLISH event { chat_id, server_id, recipients }
5. Server2/Server3 (подписаны на NATS):
   - Проверяют Redis: у нас есть соединение для этого device?
   - Если да → отправляют push через WebSocket
6. Server1 → Client: OK {server_id}
```

### Компоненты и их роли

| Компонент | Роль | Зачем нужен |
|-----------|------|-------------|
| **HAProxy** | Load balancer + sticky sessions | Распределение нагрузки, устойчивость к падению узла |
| **rchat-server (×3+)** | Gateway, бизнес-логика | Горизонтальное масштабирование |
| **PostgreSQL** | Устойчивое хранилище | Параллельные чтение/запись, репликация |
| **Redis Cluster** | Реестр соединений, presence, cache | Распределённый in-memory store |
| **NATS / Kafka** | Межсерверная доставка | Pub/Sub для push-событий |
| **S3 / MinIO** | Блобы файлов | Распределённое файловое хранилище |

---

## 5. Выбор компонентов

### 5.1. База данных: PostgreSQL vs MySQL

| Критерий | PostgreSQL | MySQL |
|----------|------------|-------|
| **JSON поддержка** | Отличная (JSONB, индексы) | Средняя (JSON без индексов до 8.0) |
| **Полнотекстовый поиск** | Встроенный (tsvector) | Требует дополнений |
| **Репликация** | Асинхронная / синхронная | Асинхронная, проще настраивать |
| **Rust драйвер** | `tokio-postgres`, `sqlx` | `mysql_async`, `sqlx` |
| **Производительность** | Лучше на сложных запросах | Быстрее на простых SELECT |
| **Лицензия** | PostgreSQL License (BSD-like) | GPL (требует FOSS или коммерческая) |

**Рекомендация:** **PostgreSQL 15+**
- Нативная поддержка JSON (ciphertext как JSONB)
- Встроенный полнотекстовый поиск (для облачных сообщений)
- `sqlx` поддерживает compile-time проверку запросов

### 5.2. In-memory store: Redis vs Memcached

| Критерий | Redis | Memcached |
|----------|-------|-----------|
| **Структуры данных** | Hash, Set, Sorted Set, Pub/Sub | Только key-value |
| **Pub/Sub** | Встроенный | Нет |
| **Кластеризация** | Redis Cluster (автошардинг) | Требует внешнего решения |
| **Персистентность** | RDB, AOF | Нет (только в памяти) |
| **Атомарные операции** | INCR, ZADD, HSET | Только CAS |

**Рекомендация:** **Redis 7+ Cluster** (3 мастера + 3 реплики)
- Pub/Sub для межсерверных событий (альтернатива NATS)
- Hash для реестра соединений: `HSET connections:{user_id} {device_id} {server_node}`
- Sorted Set для rate-limiting: `ZADD rate:{user_id} {timestamp} {request_id}`

### 5.3. Message Bus: NATS vs Kafka vs Redis Pub/Sub

| Критерий | NATS | Kafka | Redis Pub/Sub |
|----------|------|-------|---------------|
| **Задержка** | < 1ms | 5–10ms | < 1ms |
| **Персистентность** | JetStream (опционально) | Всегда | Нет (fire-and-forget) |
| **Гарантии доставки** | At-least-once (JetStream) | Exactly-once | At-most-once |
| **Сложность** | Простая | Высокая | Тривиальная |
| **Use case** | Real-time messaging | Event sourcing, logs | Простые уведомления |

**Рекомендация:** **NATS 2.10+ с JetStream**
- Низкая задержка (критично для push-доставки)
- Встроенная кластеризация (проще Kafka)
- JetStream обеспечивает персистентность (звонки не потеряются)

**Альтернатива:** Redis Pub/Sub (если Redis уже используется)
- Проще NATS, но нет гарантий доставки
- Достаточно для presence и транзиентных событий

### 5.4. Object Storage: S3 vs MinIO vs Ceph

| Критерий | AWS S3 | MinIO | Ceph |
|----------|--------|-------|------|
| **Self-hosted** | Нет (облачный) | Да | Да |
| **S3 API** | Оригинал | Совместимый | Совместимый (через RGW) |
| **Производительность** | Высокая | Высокая | Средняя (зависит от кластера) |
| **Сложность** | Тривиальная | Простая | Высокая |
| **Цена** | $0.023/ГБ/месяц | Бесплатно (self-hosted) | Бесплатно |

**Рекомендация:** **MinIO** (self-hosted) или **AWS S3** (облачный)
- MinIO: простой Docker-контейнер, S3-совместимый API
- AWS S3: надёжность 99.999999999%, но дороже для большого трафика

**Rust клиент:** `aws-sdk-s3` (работает с MinIO)

---

## 6. План миграции (7 этапов)

### Этап 0: Подготовка (1–2 недели)

**Цель:** Измерить текущую производительность, собрать метрики.

**Задачи:**
1. Добавить Prometheus-метрики в `rchat-server`:
   ```bash
   cargo add prometheus --features process
   ```
   - Активных соединений (`ACTIVE_CONNECTIONS`)
   - Latency запросов (`MESSAGE_LATENCY`, `SYNC_LATENCY`)
   - SQLite query duration (`SQLITE_QUERY_DURATION`)

2. Добавить `/metrics` endpoint:
   ```rust
   // crates/rchat-server-storage-sqlite/src/main.rs
   async fn metrics_handler() -> impl Responder {
       use prometheus::{Encoder, TextEncoder};
       let encoder = TextEncoder::new();
       let metric_families = prometheus::gather();
       let mut buffer = vec![];
       encoder.encode(&metric_families, &mut buffer).unwrap();
       String::from_utf8(buffer).unwrap()
   }
   ```

3. Запустить Grafana + Prometheus, собирать данные 1–2 недели.

4. Нагрузочное тестирование (k6 или Locust):
   ```javascript
   // load-test.js
   import ws from 'k6/ws';
   export default function() {
       ws.connect('wss://localhost:8443', function(socket) {
           socket.send(JSON.stringify({/* login */}));
           socket.on('message', (msg) => { /* handle */ });
       });
   }
   ```

**Результат:** Понимание, где узкое место (CPU, SQLite, память).

---

### Этап 1: Миграция на PostgreSQL (3–4 недели)

**Цель:** Заменить SQLite на PostgreSQL, сохранив однопроцессную модель.

**Задачи:**

1. **Создать новый крейт** `rchat-server-storage-postgres`:
   ```toml
   [dependencies]
   sqlx = { version = "0.7", features = ["postgres", "runtime-tokio-rustls", "migrate"] }
   ```

2. **Портировать схему** из SQLite в PostgreSQL:
   ```sql
   -- SQLite: INTEGER PRIMARY KEY (автоинкремент)
   -- PostgreSQL: BIGSERIAL PRIMARY KEY
   
   CREATE TABLE messages (
       server_id BIGSERIAL PRIMARY KEY,
       chat_id TEXT NOT NULL,
       sender_user_id TEXT NOT NULL,
       -- ...
   );
   ```

3. **Адаптировать запросы**:
   - SQLite: `?1, ?2` → PostgreSQL: `$1, $2`
   - SQLite: `AUTOINCREMENT` → PostgreSQL: `SERIAL` или `BIGSERIAL`
   - SQLite: `datetime('now')` → PostgreSQL: `NOW()`

4. **Миграции**:
   ```bash
   sqlx migrate add create_users_table
   sqlx migrate add create_messages_table
   # ...
   sqlx migrate run --database-url postgres://user:pass@localhost/rchat
   ```

5. **Connection pool**:
   ```rust
   let pool = sqlx::PgPool::connect_with(
       sqlx::postgres::PgConnectOptions::new()
           .host("localhost")
           .database("rchat")
           .username("rchat_user")
           .password("secret")
   )
   .max_connections(20)  // параллельные запросы
   .await?;
   ```

6. **Переключить `Repo` impl**:
   ```rust
   // main.rs
   #[cfg(feature = "postgres")]
   use rchat_server_storage_postgres::PostgresRepo;
   
   let repo: Arc<dyn Repo> = Arc::new(PostgresRepo::new(pool));
   ```

7. **Миграция данных**:
   ```bash
   # Экспорт из SQLite
   sqlite3 rchat.sqlite3 .dump > dump.sql
   # Адаптация (sed или скрипт Python)
   # Импорт в PostgreSQL
   psql -U rchat_user -d rchat -f dump_adapted.sql
   ```

8. **Тестирование**:
   ```bash
   DATABASE_URL=postgres://localhost/rchat cargo test --workspace
   ```

**Результат:** Один сервер теперь использует PostgreSQL. Параллельные чтения ускорились.

**Риски:**
- Несовместимость SQL диалектов (проверить все запросы)
- Производительность может упасть на малых данных (PostgreSQL тяжелее SQLite)

---

### Этап 2: Вынести блобы в S3/MinIO (1–2 недели)

**Цель:** Файлы больше не хранятся локально на диске сервера.

**Задачи:**

1. **Поднять MinIO** (Docker):
   ```bash
   docker run -p 9000:9000 -p 9001:9001 \
       -e MINIO_ROOT_USER=admin \
       -e MINIO_ROOT_PASSWORD=password \
       quay.io/minio/minio server /data --console-address ":9001"
   ```

2. **Добавить Rust клиент**:
   ```toml
   [dependencies]
   aws-sdk-s3 = "1.0"
   aws-config = "1.0"
   ```

3. **Реализовать `BlobStore` trait**:
   ```rust
   #[async_trait]
   pub trait BlobStore: Send + Sync {
       async fn upload_chunk(&self, blob_id: &str, data: &[u8]) -> Result<()>;
       async fn download_blob(&self, blob_id: &str) -> Result<Vec<u8>>;
       async fn delete_blob(&self, blob_id: &str) -> Result<()>;
   }
   
   pub struct S3BlobStore {
       client: aws_sdk_s3::Client,
       bucket: String,
   }
   ```

4. **Переключить `upload_blob_chunk` / `download_blob_chunk`**:
   ```rust
   // До: запись в локальный файл
   std::fs::write(format!("blobs/{}", blob_id), data)?;
   
   // После: S3 PutObject
   blob_store.upload_chunk(blob_id, data).await?;
   ```

5. **Миграция существующих блобов**:
   ```bash
   # Скрипт для загрузки локальных файлов в S3
   aws s3 sync ./blobs/ s3://rchat-blobs/
   ```

**Результат:** Файлы доступны всем серверам, нет привязки к локальному диску.

---

### Этап 3: Реестр соединений в Redis (2–3 недели)

**Цель:** Несколько серверов знают, на каком узле какое устройство.

**Задачи:**

1. **Поднять Redis Cluster**:
   ```bash
   # Docker Compose: 3 мастера + 3 реплики
   docker-compose up -d redis-cluster
   ```

2. **Rust клиент**:
   ```toml
   [dependencies]
   redis = { version = "0.24", features = ["tokio-comp", "cluster"] }
   ```

3. **Схема данных в Redis**:
   ```
   # Реестр соединений (кто где)
   HSET connections:alice desktop-1 server-1
   HSET connections:alice mobile-1 server-2
   
   # Presence (online/offline)
   ZADD presence:online 1698765432 alice  # timestamp последнего heartbeat
   
   # Rate-limiting (количество запросов)
   ZADD rate:alice:send_message 1698765432 req-123  # timestamp запроса
   ```

4. **Изменить `Connections` → `RedisConnectionRegistry`**:
   ```rust
   pub struct RedisConnectionRegistry {
       redis: redis::Client,
       node_id: String,  // уникальный идентификатор этого сервера
   }
   
   impl RedisConnectionRegistry {
       pub async fn register(&self, user_id: &str, device_id: &str) {
           let key = format!("connections:{}", user_id);
           redis::cmd("HSET")
               .arg(&key)
               .arg(device_id)
               .arg(&self.node_id)
               .query_async(&mut self.redis.get_async_connection().await?)
               .await?;
       }
       
       pub async fn lookup(&self, user_id: &str, device_id: &str) -> Option<String> {
           // Возвращает node_id сервера, где подключено устройство
       }
   }
   ```

5. **Heartbeat для presence**:
   ```rust
   // Каждые 30 секунд обновлять timestamp в Redis
   loop {
       tokio::time::sleep(Duration::from_secs(30)).await;
       redis::cmd("ZADD")
           .arg("presence:online")
           .arg(now_timestamp())
           .arg(&user_id)
           .query_async(&mut conn).await?;
   }
   
   // Считать офлайн, если timestamp старше 60 секунд
   redis::cmd("ZREMRANGEBYSCORE")
       .arg("presence:online")
       .arg("-inf")
       .arg(now_timestamp() - 60)
       .query_async(&mut conn).await?;
   ```

**Результат:** Любой сервер может узнать, где подключено устройство.

---

### Этап 4: Межсерверная доставка через NATS (3–4 недели)

**Цель:** Push-события доставляются между серверами.

**Задачи:**

1. **Поднять NATS JetStream**:
   ```bash
   docker run -p 4222:4222 nats:latest -js
   ```

2. **Rust клиент**:
   ```toml
   [dependencies]
   async-nats = "0.33"
   ```

3. **Создать стрим для событий**:
   ```rust
   let client = async_nats::connect("nats://localhost:4222").await?;
   let jetstream = async_nats::jetstream::new(client);
   
   jetstream.create_stream(async_nats::jetstream::stream::Config {
       name: "rchat-events".to_string(),
       subjects: vec!["events.>".to_string()],
       retention: async_nats::jetstream::stream::RetentionPolicy::WorkQueue,
       ..Default::default()
   }).await?;
   ```

4. **Публикация события** (отправитель):
   ```rust
   // Server-1: пользователь отправил сообщение
   let event = proto::SyncEvent {
       seq: 123,
       event: Some(proto::sync_event::Event::MessageInserted(/* ... */)),
   };
   
   jetstream.publish(
       format!("events..{}", chat_id, recipient_device_id),
       serde_json::to_vec(&event)?.into(),
   ).await?;
   ```

5. **Подписка на события** (получатель):
   ```rust
   // Server-2: слушает все события для своих соединений
   let mut subscriber = jetstream
       .subscribe("events.>")
       .await?;
   
   while let Some(msg) = subscriber.next().await {
       let event: proto::SyncEvent = serde_json::from_slice(&msg.payload)?;
       
       // Проверить: у нас есть соединение для этого устройства?
       if let Some(tx) = self.local_connections.get(&device_id) {
           tx.send(serialize_envelope(&event)).await?;
       }
       
       msg.ack().await?;  // подтвердили обработку
   }
   ```

6. **Гибридная доставка** (локальная + NATS):
   ```rust
   // При отправке сообщения:
   for recipient in recipients {
       // 1. Попробовать локальную доставку
       if let Some(tx) = self.local_connections.get(&recipient.device_id) {
           tx.send(frame).await?;
       } else {
           // 2. Проверить Redis: устройство на другом сервере?
           if let Some(node_id) = redis_registry.lookup(&recipient.device_id).await? {
               // 3. Опубликовать в NATS
               nats.publish(format!("events.{}", recipient.device_id), event).await?;
           } else {
               // Устройство офлайн, событие попадёт в журнал sync
           }
       }
   }
   ```

**Результат:** Сообщение, отправленное на Server-1, мгновенно доставляется клиенту на Server-2.

---

### Этап 5: Load Balancer и sticky sessions (1 неделя)

**Цель:** Распределять трафик между серверами, сохраняя устройство на одном узле.

**Задачи:**

1. **Настроить HAProxy**:
   ```haproxy
   # /etc/haproxy/haproxy.cfg
   frontend wss_frontend
       bind *:443 ssl crt /etc/ssl/certs/rchat.pem
       default_backend rchat_servers
   
   backend rchat_servers
       balance leastconn  # наименее загруженный сервер
       stick-table type string len 64 size 1m expire 24h
       stick on req.hdr(X-Device-Id)  # sticky по device_id из заголовка
       
       server server1 10.0.1.10:8443 check
       server server2 10.0.1.11:8443 check
       server server3 10.0.1.12:8443 check
   ```

2. **Клиент отправляет device_id в заголовке**:
   ```rust
   // rchat-transport-ws: добавить заголовок при подключении
   let mut request = tungstenite::handshake::client::Request::builder()
       .uri(url)
       .header("X-Device-Id", device_id)
       .body(())?;
   ```

3. **Health check endpoint**:
   ```rust
   // rchat-server: HTTP endpoint для HAProxy
   async fn health_check() -> &'static str {
       "OK"
   }
   ```

**Результат:** Клиенты распределяются между серверами. Sticky sessions предотвращают миграцию соединения.

---

### Этап 6: Распределённый rate-limiting (1 неделя)

**Цель:** Клиент не может обойти лимит, переключаясь между серверами.

**Задачи:**

1. **Redis Sorted Set для счётчиков**:
   ```rust
   pub async fn check_rate_limit(
       redis: &mut redis::aio::Connection,
       user_id: &str,
       limit: u32,
       window_secs: u64,
   ) -> Result<bool> {
       let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
       let key = format!("rate:{}:send_message", user_id);
       
       // Удалить старые записи
       redis::cmd("ZREMRANGEBYSCORE")
           .arg(&key)
           .arg("-inf")
           .arg(now - window_secs)
           .query_async(redis)
           .await?;
       
       // Подсчитать количество запросов в окне
       let count: u32 = redis::cmd("ZCARD")
           .arg(&key)
           .query_async(redis)
           .await?;
       
       if count >= limit {
           return Ok(false);  // лимит превышен
       }
       
       // Добавить текущий запрос
       redis::cmd("ZADD")
           .arg(&key)
           .arg(now)
           .arg(format!("{}:{}", now, rand::random::<u64>()))  // уникальный member
           .query_async(redis)
           .await?;
       
       redis::cmd("EXPIRE")
           .arg(&key)
           .arg(window_secs + 60)  // TTL с запасом
           .query_async(redis)
           .await?;
       
       Ok(true)
   }
   ```

2. **Применить в gateway**:
   ```rust
   // Перед send_message
   if !check_rate_limit(&mut redis, &user_id, 100, 60).await? {
       return Err(ServerError::RateLimitExceeded);
   }
   ```

**Результат:** Лимиты работают глобально для всех серверов.

---

### Этап 7: Мониторинг и отказоустойчивость (2–3 недели)

**Цель:** Система продолжает работать при падении одного компонента.

**Задачи:**

1. **PostgreSQL репликация**:
   ```bash
   # Primary + 2 read replicas
   docker run --name postgres-primary -e POSTGRES_PASSWORD=secret postgres:15
   docker run --name postgres-replica1 -e POSTGRES_PASSWORD=secret postgres:15
   # Настроить streaming replication
   ```

2. **Redis Cluster автофейловер**:
   - 3 мастера + 3 реплики
   - При падении мастера реплика автоматически повышается

3. **NATS cluster**:
   ```bash
   # 3 узла NATS с автоматическим переподключением
   nats-server --cluster nats://node1:6222 --routes nats://node2:6222,nats://node3:6222
   ```

4. **Graceful shutdown серверов**:
   ```rust
   // При получении SIGTERM:
   // 1. Перестать принимать новые соединения
   // 2. Отправить всем клиентам уведомление "server shutting down"
   // 3. Дождаться завершения активных запросов (timeout 30s)
   // 4. Закрыть соединения
   ```

5. **Alerting (Prometheus + Alertmanager)**:
   ```yaml
   # alerts.yml
   groups:
   - name: rchat
     rules:
     - alert: HighMessageLatency
       expr: histogram_quantile(0.99, rchat_message_latency_seconds) > 1
       for: 5m
       annotations:
         summary: "Message latency p99 > 1s"
   ```

**Результат:** Система переживает падение одного сервера/БД/Redis без downtime.

---

## 7. Оценка трудозатрат

### Временные затраты (один разработчик)

| Этап | Описание | Время | Сложность |
|------|----------|-------|-----------|
| 0 | Метрики, профилирование | 1–2 недели | Средняя |
| 1 | Миграция на PostgreSQL | 3–4 недели | Высокая |
| 2 | Блобы в S3/MinIO | 1–2 недели | Низкая |
| 3 | Реестр соединений в Redis | 2–3 недели | Средняя |
| 4 | Межсерверная доставка (NATS) | 3–4 недели | Высокая |
| 5 | Load Balancer + sticky | 1 неделя | Низкая |
| 6 | Распределённый rate-limit | 1 неделя | Средняя |
| 7 | Мониторинг, failover | 2–3 недели | Средняя |
| **Итого** | | **14–20 недель** | |

### Команда из 2–3 разработчиков

- Параллельная работа сокращает до **10–12 недель** (2.5–3 месяца)
- Один работает над БД (этапы 1–2)
- Второй — над Redis/NATS (этапы 3–4)
- Третий — инфраструктура (этапы 5–7)

### Дополнительные затраты

1. **DevOps-инженер** для настройки кластеров (1–2 месяца)
2. **QA-тестирование** под нагрузкой (2–3 недели)
3. **Документация** операционных процедур (1 неделя)

**Общая оценка:** **3–6 месяцев** для полной миграции.

---

## 8. Альтернативные стратегии

### Стратегия A: Вертикальное масштабирование

**Идея:** Запустить сервер на мощной машине вместо кластера.

**Преимущества:**
- ✅ Не нужно менять код (используем текущий SQLite)
- ✅ Проще отладка
- ✅ Нет сложности распределённых систем

**Как:**
- Арендовать AWS EC2 c5.9xlarge (36 vCPU, 72 ГБ RAM)
- Увеличить `ulimit -n 100000` (дескрипторы)
- SQLite на NVMe SSD (до 1M IOPS)

**Предел:** ~50,000 одновременных соединений на одной машине.

**Недостатки:**
- ❌ Single point of failure (падение = полный downtime)
- ❌ Дорого (машина стоит $1,500+/месяц)
- ❌ Нет географического распределения

**Когда выбирать:** Если пользователей < 50,000 и недоступность 1 час/месяц приемлема.

---

### Стратегия B: Read Replicas (PostgreSQL) без кластера

**Идея:** Один primary для записи, несколько replicas для чтения.

**Архитектура:**
```
            Primary (записи)
               ↓
        ┌──────┴──────┐
        ↓              ↓
    Replica1      Replica2   (чтения: история, поиск)
```

**Преимущества:**
- ✅ Ускоряет `get_history` и `search_messages` (чтения на replicas)
- ✅ Проще полного кластера (нет NATS, Redis)
- ✅ Все серверы пишут в один primary (нет проблемы распределённой доставки)

**Недостатки:**
- ❌ Primary остаётся узким местом для записи
- ❌ Всё ещё нужен Redis для реестра соединений

**Когда выбирать:** Если узкое место — чтение истории (90% запросов), а записей мало.

---

### Стратегия C: Региональные кластеры (geo-distribution)

**Идея:** Отдельный кластер в каждом регионе (US, EU, Asia).

**Архитектура:**
```
US Cluster                 EU Cluster                Asia Cluster
├─ Server1,2,3             ├─ Server1,2,3           ├─ Server1,2,3
├─ PostgreSQL              ├─ PostgreSQL            ├─ PostgreSQL
└─ Redis                   └─ Redis                 └─ Redis
       │                          │                        │
       └──────────── Cross-region sync ───────────────────┘
                    (CockroachDB или Cassandra)
```

**Преимущества:**
- ✅ Низкая задержка для всех пользователей (подключаются к ближайшему кластеру)
- ✅ Отказоустойчивость на уровне региона

**Недостатки:**
- ❌ Очень сложно: нужна распределённая БД (CockroachDB, YugabyteDB, Cassandra)
- ❌ Конфликты при записи в разных регионах (eventual consistency)

**Когда выбирать:** Только для глобального продукта (миллионы пользователей в разных странах).

---

## 9. Чеклист готовности

### Перед началом миграции

- [ ] **Метрики собраны** минимум 2 недели (знаем узкие места)
- [ ] **Backup базы данных** автоматизирован (PostgreSQL pg_dump каждые 6 часов)
- [ ] **Тесты покрывают** критические пути (отправка, синхронизация, E2E)
- [ ] **Staging-окружение** идентично production (та же инфраструктура)
- [ ] **Rollback-план** документирован (как вернуться на SQLite за 1 час)
- [ ] **Команда готова** поддерживать кластер (знает Redis, NATS, PostgreSQL)

### После каждого этапа

- [ ] **Нагрузочное тестирование** прошло (k6: 10,000 соединений, 1000 сообщений/сек)
- [ ] **Latency не выросла** (p99 ≤ 500ms)
- [ ] **Все E2E-тесты прошли** (`cargo test --workspace`)
- [ ] **Документация обновлена** (`docs/deployment.md`)
- [ ] **Monitoring настроен** (Grafana дашборды, алерты)

### Финальная проверка

- [ ] **Failover тест:** Убить один сервер → клиенты переподключаются к другому
- [ ] **Chaos test:** Убить Redis → система деградирует gracefully (push не работает, но сообщения сохраняются)
- [ ] **Split-brain test:** Разорвать сеть между PostgreSQL primary и replicas → реплики не принимают запись
- [ ] **Load test:** 50,000 соединений, 10,000 сообщений/сек в течение 1 часа

---

## Итоговые рекомендации

### Если пользователей < 10,000
→ **Оставайтесь на однопроцессной модели.** Оптимизируйте:
- Индексы в SQLite (`CREATE INDEX idx_messages_chat_server ON messages(chat_id, server_id DESC)`)
- Уменьшить размер `sync_events` (удалять события старше 30 дней)
- Кеширование частых запросов (список чатов пользователя)

### Если пользователей 10,000–50,000
→ **Вертикальное масштабирование + Read Replicas:**
1. Мигрировать на PostgreSQL (этап 1)
2. Добавить 2 read replicas для истории
3. Мощный сервер (16+ vCPU, 32+ ГБ RAM)

### Если пользователей > 50,000 или требуется 99.9% uptime
→ **Полная миграция на кластер** (этапы 1–7).

### Если нужна геораспределённость
→ **Региональные кластеры** (стратегия C).

---

## Полезные ссылки

**PostgreSQL:**
- [Документация по репликации](https://www.postgresql.org/docs/current/warm-standby.html)
- [sqlx миграции](https://github.com/launchbadge/sqlx/blob/main/sqlx-cli/README.md)

**Redis:**
- [Redis Cluster Tutorial](https://redis.io/docs/management/scaling/)
- [Rust клиент redis-rs](https://github.com/redis-rs/redis-rs)

**NATS:**
- [JetStream Guide](https://docs.nats.io/nats-concepts/jetstream)
- [Rust async-nats](https://github.com/nats-io/nats.rs)

**HAProxy:**
- [Sticky Sessions](https://www.haproxy.com/blog/load-balancing-affinity-persistence-sticky-sessions-what-you-need-to-know)

**Нагрузочное тестирование:**
- [k6 для WebSocket](https://k6.io/docs/using-k6/protocols/websockets/)

---

## Заключение

Миграция на кластер — это **3–6 месяцев работы** команды и **значительное усложнение
инфраструктуры**. Масштабируйте, только когда **измерения показывают проблему**,
а не заранее.

Однопроцессная архитектура rchat способна обслуживать **тысячи пользователей**
без изменений. Когда придёт время масштабировать — этот документ даст чёткий
план действий.
