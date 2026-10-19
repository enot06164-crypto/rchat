//! Реестр активных звонков (только сигнализация).
//!
//! Состояние живёт в памяти процесса: звонки эфемерны, не журналируются и
//! не переживают рестарт сервера. Реестр не знает про сеть — он лишь хранит
//! переходы состояния; доставку push-событий выполняет [`crate::service`].
//!
//! Инварианты:
//! - у пользователя не больше одного звонка (ringing или active) одновременно;
//! - звонок существует только в личном чате (проверяет сервис);
//! - принять/отклонить может только callee, завершить — любой участник;
//! - ringing истекает через [`RING_TIMEOUT_MS`], active — через
//!   [`ACTIVE_TIMEOUT_MS`] после принятия; истёкшие звонки удаляются
//!   `expire` (сервис рассылает по ним событие `ENDED`).

use std::collections::HashMap;
use std::sync::Mutex;

/// Сколько звонок может звонить до автоистечения.
pub const RING_TIMEOUT_MS: u64 = 60_000;
/// Верхняя граница активного звонка (медиа нет; защита от «вечного» вызова).
pub const ACTIVE_TIMEOUT_MS: u64 = 4 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    Ringing,
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveCall {
    pub call_id: String,
    pub chat_id: String,
    pub caller_user_id: String,
    pub callee_user_id: String,
    pub state: CallState,
    pub created_at_ms: u64,
    /// 0, пока звонок не принят.
    pub accepted_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallError {
    /// У одного из участников уже есть звонок.
    Busy,
    NotFound,
    /// Пользователь не участник звонка (или не callee для accept/decline).
    Forbidden,
    /// Отклонить можно только звонящий звонок.
    NotRinging,
}

#[derive(Default)]
pub struct CallRegistry {
    inner: Mutex<HashMap<String, ActiveCall>>,
}

impl CallRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Удаляет истёкшие звонки и возвращает их для рассылки `ENDED`.
    pub fn expire(&self, now_ms: u64) -> Vec<ActiveCall> {
        let mut inner = self.inner.lock().expect("calls poisoned");
        let mut expired = Vec::new();
        inner.retain(|_, call| {
            let since_ms = match call.state {
                CallState::Ringing => call.created_at_ms,
                CallState::Active => call.accepted_at_ms,
            };
            let ttl = match call.state {
                CallState::Ringing => RING_TIMEOUT_MS,
                CallState::Active => ACTIVE_TIMEOUT_MS,
            };
            let alive = now_ms.saturating_sub(since_ms) < ttl;
            if !alive {
                expired.push(call.clone());
            }
            alive
        });
        expired.sort_by_key(|call| call.created_at_ms);
        expired
    }

    /// Создаёт ringing-звонок. `call_id` генерирует вызывающий (сервис).
    pub fn invite(
        &self,
        call_id: &str,
        chat_id: &str,
        caller_user_id: &str,
        callee_user_id: &str,
        now_ms: u64,
    ) -> Result<ActiveCall, CallError> {
        let mut inner = self.inner.lock().expect("calls poisoned");
        let busy = inner.values().any(|call| {
            call.caller_user_id == caller_user_id
                || call.callee_user_id == caller_user_id
                || call.caller_user_id == callee_user_id
                || call.callee_user_id == callee_user_id
        });
        if busy {
            return Err(CallError::Busy);
        }
        let call = ActiveCall {
            call_id: call_id.to_string(),
            chat_id: chat_id.to_string(),
            caller_user_id: caller_user_id.to_string(),
            callee_user_id: callee_user_id.to_string(),
            state: CallState::Ringing,
            created_at_ms: now_ms,
            accepted_at_ms: 0,
        };
        inner.insert(call_id.to_string(), call.clone());
        Ok(call)
    }

    /// Принимает ringing-звонок (идемпотентно для уже активного).
    pub fn accept(
        &self,
        call_id: &str,
        user_id: &str,
        now_ms: u64,
    ) -> Result<ActiveCall, CallError> {
        let mut inner = self.inner.lock().expect("calls poisoned");
        let call = inner.get_mut(call_id).ok_or(CallError::NotFound)?;
        if call.callee_user_id != user_id {
            return Err(CallError::Forbidden);
        }
        if call.state == CallState::Ringing {
            call.state = CallState::Active;
            call.accepted_at_ms = now_ms;
        }
        Ok(call.clone())
    }

    /// Отклоняет ringing-звонок; удаляет его из реестра.
    pub fn decline(&self, call_id: &str, user_id: &str) -> Result<ActiveCall, CallError> {
        let mut inner = self.inner.lock().expect("calls poisoned");
        let call = inner.get(call_id).ok_or(CallError::NotFound)?;
        if call.callee_user_id != user_id {
            return Err(CallError::Forbidden);
        }
        if call.state != CallState::Ringing {
            return Err(CallError::NotRinging);
        }
        let call = inner.remove(call_id).expect("checked above");
        Ok(call)
    }

    /// Завершает звонок (участник — caller или callee); удаляет его.
    pub fn end(&self, call_id: &str, user_id: &str) -> Result<ActiveCall, CallError> {
        let mut inner = self.inner.lock().expect("calls poisoned");
        let call = inner.get(call_id).ok_or(CallError::NotFound)?;
        if call.caller_user_id != user_id && call.callee_user_id != user_id {
            return Err(CallError::Forbidden);
        }
        let call = inner.remove(call_id).expect("checked above");
        Ok(call)
    }

    /// Удаляет все звонки пользователя (например, при уходе в офлайн).
    pub fn end_for_user(&self, user_id: &str) -> Vec<ActiveCall> {
        let mut inner = self.inner.lock().expect("calls poisoned");
        let ids: Vec<String> = inner
            .values()
            .filter(|call| call.caller_user_id == user_id || call.callee_user_id == user_id)
            .map(|call| call.call_id.clone())
            .collect();
        let mut ended: Vec<ActiveCall> =
            ids.into_iter().filter_map(|id| inner.remove(&id)).collect();
        ended.sort_by_key(|call| call.created_at_ms);
        ended
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().expect("calls poisoned").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite(registry: &CallRegistry, id: &str, caller: &str, callee: &str) -> ActiveCall {
        registry
            .invite(id, "chat-1", caller, callee, 1_000)
            .expect("invite")
    }

    #[test]
    fn invite_accept_end_transitions() {
        let registry = CallRegistry::new();
        let call = invite(&registry, "call-1", "alice", "bob");
        assert_eq!(call.state, CallState::Ringing);

        // Принять может только callee.
        assert_eq!(
            registry.accept("call-1", "alice", 2_000),
            Err(CallError::Forbidden)
        );
        let accepted = registry.accept("call-1", "bob", 2_000).expect("accept");
        assert_eq!(accepted.state, CallState::Active);
        assert_eq!(accepted.accepted_at_ms, 2_000);
        // Повторное принятие идемпотентно.
        assert_eq!(
            registry
                .accept("call-1", "bob", 3_000)
                .expect("accept again"),
            accepted
        );

        // Отклонить активный звонок нельзя.
        assert_eq!(
            registry.decline("call-1", "bob"),
            Err(CallError::NotRinging)
        );
        // Завершить может любой участник.
        let ended = registry.end("call-1", "alice").expect("end");
        assert_eq!(ended.state, CallState::Active);
        assert_eq!(registry.len(), 0);
        assert_eq!(registry.end("call-1", "alice"), Err(CallError::NotFound));
    }

    #[test]
    fn decline_requires_ringing_and_callee() {
        let registry = CallRegistry::new();
        invite(&registry, "call-1", "alice", "bob");
        assert_eq!(
            registry.decline("call-1", "alice"),
            Err(CallError::Forbidden)
        );
        let declined = registry.decline("call-1", "bob").expect("decline");
        assert_eq!(declined.state, CallState::Ringing);
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn busy_user_cannot_have_two_calls() {
        let registry = CallRegistry::new();
        invite(&registry, "call-1", "alice", "bob");
        // Bob занят; Alice занята; Carol свободна, но её звонок Dave конфликтует
        // только с ними, не с Alice/Bob.
        assert_eq!(
            registry.invite("call-2", "chat-2", "bob", "carol", 1_000),
            Err(CallError::Busy)
        );
        assert_eq!(
            registry.invite("call-2", "chat-2", "carol", "alice", 1_000),
            Err(CallError::Busy)
        );
        assert!(registry
            .invite("call-2", "chat-2", "carol", "dave", 1_000)
            .is_ok());
    }

    #[test]
    fn expire_ringing_and_active() {
        let registry = CallRegistry::new();
        invite(&registry, "call-1", "alice", "bob");
        // created_at_ms = 1_000, поэтому отсчёт идёт от него.
        assert!(registry.expire(1_000 + RING_TIMEOUT_MS - 1).is_empty());
        let expired = registry.expire(1_000 + RING_TIMEOUT_MS);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].call_id, "call-1");
        assert_eq!(registry.len(), 0);

        // Active живёт отсчёт от accepted_at_ms, а не от created_at_ms.
        registry
            .invite("call-2", "chat-1", "alice", "bob", 1_000)
            .expect("invite");
        registry.accept("call-2", "bob", 50_000).expect("accept");
        assert!(registry.expire(50_000 + ACTIVE_TIMEOUT_MS - 1).is_empty());
        assert_eq!(registry.expire(50_000 + ACTIVE_TIMEOUT_MS).len(), 1);
    }

    #[test]
    fn end_for_user_removes_only_their_calls() {
        let registry = CallRegistry::new();
        invite(&registry, "call-1", "alice", "bob");
        // Второй звонок невозможен, пока alice/bob заняты; эмулируем снятие.
        registry.end("call-1", "alice").expect("end");
        invite(&registry, "call-2", "carol", "dave");
        assert_eq!(registry.end_for_user("bob").len(), 0);
        let ended = registry.end_for_user("carol");
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].call_id, "call-2");
    }
}
