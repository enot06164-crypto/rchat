//! Обёртка над транспортом с управляемыми помехами.
//!
//! [`MockTransportFactory`] оборачивает любую `TransportFactory` (например,
//! каналы в памяти из [`crate::server`] или WebSocket) и позволяет тесту
//! детерминированно имитировать:
//!
//! - задержки доставки ([`FaultHandle::delay_next_inbound`],
//!   [`FaultHandle::delay_next_send`]);
//! - разрыв и ошибки ([`FaultHandle::drop_next_inbound`],
//!   [`FaultHandle::fail_next_inbound`], [`FaultHandle::fail_next_send`]);
//! - повтор кадров и управление порядком ([`FaultHandle::duplicate_next_inbound`],
//!   [`FaultHandle::inject_inbound`]).
//!
//! Помехи хранятся в очереди и применяются к следующему вызову `recv`/`send`
//! активного соединения в порядке постановки. Очередь переживает реконнект
//! (пока не опустеет): сбой можно запланировать даже во время простоя.
//! Внутренний транспорт обязан быть cancel-safe в `recv` (каналы tokio — да);
//! это условие описано в `docs/testing.md`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rchat_core::error::{TransportError, TransportErrorKind};
use rchat_core::traits::{Transport, TransportFactory};
use tokio::sync::Notify;

/// Помеха входящего направления.
#[derive(Debug, Clone)]
enum InboundFault {
    Delay(Duration),
    /// Корректное закрытие соединения (`Ok(None)`).
    Disconnect,
    Fail,
    Duplicate,
    Inject(Vec<u8>),
}

/// Помеха исходящего направления.
#[derive(Debug, Clone)]
enum OutboundFault {
    Delay(Duration),
    Fail,
}

#[derive(Default)]
struct SharedState {
    inbound: Mutex<VecDeque<InboundFault>>,
    inbound_notify: Notify,
    outbound: Mutex<VecDeque<OutboundFault>>,
    sent: Mutex<Vec<Vec<u8>>>,
    connections: AtomicUsize,
}

/// Разделяемая с тестом ручка управления помехами.
///
/// Дешёвая в клонировании; помехи влияют на активное соединение фабрики.
#[derive(Clone)]
pub struct FaultHandle {
    state: Arc<SharedState>,
}

impl FaultHandle {
    /// Следующий входящий кадр не придёт: соединение «закрыто» сервером.
    pub fn drop_next_inbound(&self) {
        self.push_inbound(InboundFault::Disconnect);
    }

    /// Следующий `recv` вернёт ошибку транспорта.
    pub fn fail_next_inbound(&self) {
        self.push_inbound(InboundFault::Fail);
    }

    /// Задержать следующий входящий кадр на `delay`.
    pub fn delay_next_inbound(&self, delay: Duration) {
        self.push_inbound(InboundFault::Delay(delay));
    }

    /// Следующий входящий кадр будет выдан дважды (повтор фрейма).
    pub fn duplicate_next_inbound(&self) {
        self.push_inbound(InboundFault::Duplicate);
    }

    /// Внедрить произвольный кадр, не читая транспорт (контроль порядка,
    /// битые/обрезанные пакеты). Выдаётся при ближайшем `recv`.
    pub fn inject_inbound(&self, frame: Vec<u8>) {
        self.push_inbound(InboundFault::Inject(frame));
    }

    /// Следующий `send` завершится ошибкой.
    pub fn fail_next_send(&self) {
        self.state
            .outbound
            .lock()
            .expect("mock transport poisoned")
            .push_back(OutboundFault::Fail);
    }

    /// Задержать следующий `send`.
    pub fn delay_next_send(&self, delay: Duration) {
        self.state
            .outbound
            .lock()
            .expect("mock transport poisoned")
            .push_back(OutboundFault::Delay(delay));
    }

    /// Снять все неиспользованные помехи.
    pub fn clear(&self) {
        self.state
            .inbound
            .lock()
            .expect("mock transport poisoned")
            .clear();
        self.state
            .outbound
            .lock()
            .expect("mock transport poisoned")
            .clear();
    }

    /// Кадры, успешно отправленные клиентом через этот транспорт.
    pub fn sent_frames(&self) -> Vec<Vec<u8>> {
        self.state
            .sent
            .lock()
            .expect("mock transport poisoned")
            .clone()
    }

    /// Сколько соединений открыто фабрикой (реконнекты видны по росту).
    pub fn connections(&self) -> usize {
        self.state.connections.load(Ordering::SeqCst)
    }

    fn push_inbound(&self, fault: InboundFault) {
        self.state
            .inbound
            .lock()
            .expect("mock transport poisoned")
            .push_back(fault);
        self.state.inbound_notify.notify_one();
    }
}

/// Фабрика транспорта с помехами; при пустой очереди проксирует вызовы.
pub struct MockTransportFactory {
    inner: Arc<dyn TransportFactory>,
    state: Arc<SharedState>,
}

impl MockTransportFactory {
    pub fn new(inner: Arc<dyn TransportFactory>) -> Self {
        Self {
            inner,
            state: Arc::new(SharedState::default()),
        }
    }

    /// Ручка управления помехами; действует на текущее и будущие соединения.
    pub fn faults(&self) -> FaultHandle {
        FaultHandle {
            state: Arc::clone(&self.state),
        }
    }

    /// Сколько соединений открыто (включая реконнекты).
    pub fn connections(&self) -> usize {
        self.state.connections.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl TransportFactory for MockTransportFactory {
    async fn connect(&self) -> Result<Box<dyn Transport>, TransportError> {
        let inner = self.inner.connect().await?;
        self.state.connections.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(MockTransport {
            inner,
            state: Arc::clone(&self.state),
            repeat: None,
        }))
    }
}

fn mock_error(message: &str) -> TransportError {
    TransportError {
        kind: TransportErrorKind::Other,
        message: message.to_string(),
    }
}

/// Транспорт с помехами; см. [`MockTransportFactory`].
struct MockTransport {
    inner: Box<dyn Transport>,
    state: Arc<SharedState>,
    /// Кадр, который нужно выдать повторно (после `Duplicate`).
    repeat: Option<Vec<u8>>,
}

impl MockTransport {
    fn pop_inbound(&self) -> Option<InboundFault> {
        self.state
            .inbound
            .lock()
            .expect("mock transport poisoned")
            .pop_front()
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        let fault = {
            self.state
                .outbound
                .lock()
                .expect("mock transport poisoned")
                .pop_front()
        };
        match fault {
            Some(OutboundFault::Delay(delay)) => tokio::time::sleep(delay).await,
            Some(OutboundFault::Fail) => {
                return Err(mock_error("injected outbound transport failure"));
            }
            None => {}
        }
        self.inner.send(frame).await?;
        self.state
            .sent
            .lock()
            .expect("mock transport poisoned")
            .push(frame.to_vec());
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        if let Some(frame) = self.repeat.take() {
            return Ok(Some(frame));
        }
        loop {
            if let Some(fault) = self.pop_inbound() {
                match fault {
                    InboundFault::Disconnect => return Ok(None),
                    InboundFault::Fail => {
                        return Err(mock_error("injected inbound transport failure"));
                    }
                    InboundFault::Delay(delay) => tokio::time::sleep(delay).await,
                    InboundFault::Inject(frame) => return Ok(Some(frame)),
                    InboundFault::Duplicate => {
                        let frame = self.inner.recv().await?;
                        self.repeat = frame.clone();
                        return Ok(frame);
                    }
                }
                continue;
            }
            tokio::select! {
                biased;
                _ = self.state.inbound_notify.notified() => {}
                result = self.inner.recv() => return result,
            }
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close().await
    }
}
