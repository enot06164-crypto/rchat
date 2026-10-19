//! C-ABI и JSON-интерфейс клиента rchat (стиль tdjson).
//!
//! Один [`RchatHandle`] — один независимый клиент/профиль. Функции
//! `rchat_send_json` синхронно выполняют запрос и возвращают JSON-ответ,
//! `rchat_receive_json` забирает события из очереди. Формат описан в
//! `docs/ffi.md`; C-заголовок — `include/rchat.h`.
//!
//! # Потоки и память
//!
//! * Handle можно использовать из нескольких потоков одновременно;
//!   `rchat_send_json` блокирует вызывающий поток до ответа сервера.
//! * `rchat_destroy` нельзя вызывать, пока другие потоки используют handle.
//! * Строки, возвращённые `rchat_send_json`/`rchat_receive_json`, освобождает
//!   вызывающая сторона через `rchat_free_string`. `rchat_version` и
//!   `rchat_last_error` возвращают заимствованные строки — их освобождать
//!   нельзя.
//! * Паники Rust не переходят через C-границу: они превращаются в JSON-ошибку
//!   или NULL.
//!
//! Встраивание из Rust: не вызывайте эти функции из потока tokio-runtime —
//! внутри используется собственный runtime и `block_on`.

mod commands;
mod config;
mod error;
mod event;
mod json;
pub mod schema;

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rchat_client_storage_sqlite::ClientSqliteStorage;
use rchat_core::client::Client;
use serde_json::Value;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use crate::error::FfiError;

/// Версия JSON-схемы, см. `docs/ffi.md`.
pub const API_VERSION: u32 = commands::API_VERSION;

/// Непрозрачный handle клиента. Создаётся `rchat_create`, освобождается
/// `rchat_destroy`.
pub struct RchatHandle {
    pub(crate) client: Client,
    pub(crate) storage: Arc<ClientSqliteStorage>,
    pub(crate) data_dir: PathBuf,
    pub(crate) server: String,
    pub(crate) e2e: bool,
    pub(crate) device_name: String,
    pub(crate) events: tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>,
    pub(crate) dropped_events: Arc<AtomicU64>,
    pub(crate) event_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();

pub(crate) fn runtime() -> Result<&'static Runtime, String> {
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("rchat-ffi")
                .enable_all()
                .build()
                .map_err(|error| format!("tokio runtime: {error}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn clear_last_error() {
    LAST_ERROR.with(|cell| *cell.borrow_mut() = None);
}

fn set_last_error(message: &str) {
    let cleaned = message.replace('\0', " ");
    let value = CString::new(cleaned)
        .unwrap_or_else(|_| CString::new("rchat-ffi error").expect("static string"));
    LAST_ERROR.with(|cell| *cell.borrow_mut() = Some(value));
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        format!("panic: {message}")
    } else if let Some(message) = payload.downcast_ref::<String>() {
        format!("panic: {message}")
    } else {
        "panic in rchat-ffi".to_string()
    }
}

fn into_c_string(value: String) -> *mut c_char {
    match CString::new(value) {
        Ok(value) => value.into_raw(),
        Err(_) => CString::new(
            "{\"@type\":\"error\",\"code\":\"internal\",\"message\":\"NUL in response\"}",
        )
        .expect("static JSON")
        .into_raw(),
    }
}

fn create_inner(config_json: *const c_char) -> Result<RchatHandle, String> {
    if config_json.is_null() {
        return Err("config_json is null".to_string());
    }
    // SAFETY: вызывающая сторона обязана передать валидную NUL-терминированную
    // строку; функция вызывается из `rchat_create` (unsafe extern "C").
    let text = unsafe { CStr::from_ptr(config_json) }
        .to_str()
        .map_err(|error| format!("config is not UTF-8: {error}"))?;
    let config = config::FfiConfig::parse(text)?;
    config::build_handle(config)
}

fn send_inner(handle: *mut RchatHandle, request_json: *const c_char) -> Value {
    if handle.is_null() {
        return FfiError::invalid_handle().to_response(None);
    }
    // SAFETY: handle создан `rchat_create` и не освобождён (контракт C-ABI).
    let handle = unsafe { &*handle };
    if request_json.is_null() {
        return FfiError::invalid("request_json is null").to_response(None);
    }
    // SAFETY: см. контракт функции.
    let text = match unsafe { CStr::from_ptr(request_json) }.to_str() {
        Ok(text) => text,
        Err(error) => {
            return FfiError::invalid(format!("request is not UTF-8: {error}")).to_response(None)
        }
    };
    let request: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => return FfiError::invalid(format!("request JSON: {error}")).to_response(None),
    };
    if !request.is_object() {
        return FfiError::invalid("request must be a JSON object").to_response(None);
    }
    let method = match request.get("@type").and_then(Value::as_str) {
        Some(method) => method,
        None => {
            return FfiError::invalid("missing string field @type").to_response(None);
        }
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => return FfiError::new("internal", error).to_response(Some(method)),
    };
    match runtime.block_on(commands::dispatch(handle, method, &request)) {
        Ok(data) => serde_json::json!({
            "@type": "ok",
            "request": method,
            "data": data,
        }),
        Err(error) => error.to_response(Some(method)),
    }
}

fn receive_inner(handle: *mut RchatHandle, timeout_ms: u64) -> Option<Vec<u8>> {
    if handle.is_null() {
        return None;
    }
    // SAFETY: handle создан `rchat_create` и не освобождён (контракт C-ABI).
    let handle = unsafe { &*handle };
    let runtime = runtime().ok()?;
    // Потери сигнализируются до следующего события: очередь уже переполнена,
    // и хост должен узнать об этом как можно раньше.
    let missed = handle.dropped_events.swap(0, Ordering::Relaxed);
    if missed > 0 {
        return Some(json::lagged_event(missed));
    }
    runtime.block_on(async {
        let mut events = handle.events.lock().await;
        if timeout_ms == 0 {
            match events.try_recv() {
                Ok(event) => return Some(event),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return None,
            }
        } else {
            let timeout = Duration::from_millis(timeout_ms.min(86_400_000));
            match tokio::time::timeout(timeout, events.recv()).await {
                Ok(Some(event)) => return Some(event),
                Ok(None) => return None,
                Err(_) => {}
            }
        }
        let missed = handle.dropped_events.swap(0, Ordering::Relaxed);
        (missed > 0).then(|| json::lagged_event(missed))
    })
}

fn destroy_inner(handle: *mut RchatHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: handle получен из `Box::into_raw` в `rchat_create` и ещё не
    // освобождён; другие потоки не используют его (контракт C-ABI).
    let handle = unsafe { Box::from_raw(handle) };
    if let Ok(mut task) = handle.event_task.lock() {
        if let Some(task) = task.take() {
            task.abort();
        }
    }
    if let Ok(runtime) = runtime() {
        let _ = runtime.block_on(handle.client.shutdown());
    }
    drop(handle);
}

/// Создаёт клиент по JSON-конфигурации. При ошибке возвращает NULL; текст
/// ошибки доступен через `rchat_last_error`.
///
/// # Safety
///
/// `config_json` должен указывать на валидную NUL-терминированную UTF-8
/// строку или быть NULL.
#[no_mangle]
pub unsafe extern "C" fn rchat_create(config_json: *const c_char) -> *mut RchatHandle {
    clear_last_error();
    match catch_unwind(AssertUnwindSafe(|| create_inner(config_json))) {
        Ok(Ok(handle)) => Box::into_raw(Box::new(handle)),
        Ok(Err(error)) => {
            set_last_error(&error);
            std::ptr::null_mut()
        }
        Err(payload) => {
            let message = panic_message(payload);
            set_last_error(&message);
            std::ptr::null_mut()
        }
    }
}

/// Уничтожает клиент, останавливает фоновые задачи и освобождает память.
/// NULL допустим. После вызова handle нельзя использовать.
///
/// # Safety
///
/// `handle` должен быть создан `rchat_create` и не освобождён; другие потоки
/// не должны обращаться к нему во время вызова.
#[no_mangle]
pub unsafe extern "C" fn rchat_destroy(handle: *mut RchatHandle) {
    let _ = catch_unwind(AssertUnwindSafe(|| destroy_inner(handle)));
}

/// Выполняет JSON-запрос. Возвращает JSON-ответ, который нужно освободить
/// `rchat_free_string`. NULL не возвращается (кроме катастрофического сбоя
/// выделения памяти).
///
/// # Safety
///
/// `handle` должен быть валиден; `request_json` — NUL-терминированная
/// UTF-8 JSON-строка.
#[no_mangle]
pub unsafe extern "C" fn rchat_send_json(
    handle: *mut RchatHandle,
    request_json: *const c_char,
) -> *mut c_char {
    let value = match catch_unwind(AssertUnwindSafe(|| send_inner(handle, request_json))) {
        Ok(value) => value,
        Err(payload) => {
            let message = panic_message(payload);
            set_last_error(&message);
            FfiError::new("panic", message).to_response(None)
        }
    };
    match serde_json::to_string(&value) {
        Ok(text) => into_c_string(text),
        Err(error) => into_c_string(
            FfiError::new("internal", format!("response serialization: {error}"))
                .to_response(None)
                .to_string(),
        ),
    }
}

/// Забирает событие из очереди, ожидая до `timeout_ms` (0 — без ожидания).
/// Возвращает NULL по тайм-ауту/пустой очереди. Строку освобождает
/// `rchat_free_string`.
///
/// # Safety
///
/// `handle` должен быть валиден.
#[no_mangle]
pub unsafe extern "C" fn rchat_receive_json(
    handle: *mut RchatHandle,
    timeout_ms: u64,
) -> *mut c_char {
    match catch_unwind(AssertUnwindSafe(|| receive_inner(handle, timeout_ms))) {
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(text) => into_c_string(text),
            Err(error) => {
                set_last_error(&error.to_string());
                std::ptr::null_mut()
            }
        },
        Ok(None) => std::ptr::null_mut(),
        Err(payload) => {
            set_last_error(&panic_message(payload));
            std::ptr::null_mut()
        }
    }
}

/// Освобождает строку, возвращённую `rchat_send_json`/`rchat_receive_json`.
/// NULL допустим.
///
/// # Safety
///
/// `value` должен быть получен из этого API и ещё не освобождён.
#[no_mangle]
pub unsafe extern "C" fn rchat_free_string(value: *mut c_char) {
    if !value.is_null() {
        // SAFETY: строка создана `CString::into_raw` в этом API.
        drop(unsafe { CString::from_raw(value) });
    }
}

/// Версия библиотеки. Заимствованная статическая строка; освобождать нельзя.
#[no_mangle]
pub extern "C" fn rchat_version() -> *const c_char {
    const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");
    VERSION.as_ptr().cast()
}

/// Версия JSON-схемы.
#[no_mangle]
pub extern "C" fn rchat_api_version() -> u32 {
    API_VERSION
}

/// Последняя ошибка `rchat_create` на этом потоке. Заимствованная строка,
/// действует до следующего вызова, меняющего ошибку на этом потоке; NULL,
/// если ошибки нет.
#[no_mangle]
pub extern "C" fn rchat_last_error() -> *const c_char {
    LAST_ERROR.with(|cell| match cell.borrow().as_ref() {
        Some(message) => message.as_ptr(),
        None => std::ptr::null(),
    })
}
