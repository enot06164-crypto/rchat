//! Простой лимитер частоты (скользящее окно) для регистрации, входа и отправки.
//!
//! Состояние — в памяти процесса: для одного экземпляра сервера это
//! достаточно и документировано; при нескольких экземплярах потребуется
//! общий счётчик.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::RateLimits;

#[derive(Default)]
struct Window {
    events: VecDeque<Instant>,
}

struct Inner {
    buckets: HashMap<String, Window>,
}

/// Лимитер с несколькими именованными окнами.
pub struct RateLimiter {
    inner: Mutex<Inner>,
    pub limits: RateLimits,
}

impl RateLimiter {
    pub fn new(limits: RateLimits) -> Self {
        Self {
            inner: Mutex::new(Inner {
                buckets: HashMap::new(),
            }),
            limits,
        }
    }

    /// Регистрирует попытку и сообщает, разрешена ли она.
    pub fn check(&self, key: &str, limit: u32, window: Duration) -> bool {
        self.check_at(key, limit, window, Instant::now())
    }

    /// Версия с явным «сейчас» — для детерминированных тестов.
    pub fn check_at(&self, key: &str, limit: u32, window: Duration, now: Instant) -> bool {
        if limit == 0 {
            return false;
        }
        let mut inner = self.inner.lock().expect("rate limiter poisoned");
        let bucket = inner.buckets.entry(key.to_string()).or_default();
        while let Some(front) = bucket.events.front() {
            if now.duration_since(*front) > window {
                bucket.events.pop_front();
            } else {
                break;
            }
        }
        if bucket.events.len() as u32 >= limit {
            return false;
        }
        bucket.events.push_back(now);
        true
    }

    /// Удаляет старые записи, чтобы карта не росла бесконечно.
    pub fn gc(&self, now: Instant, max_age: Duration) {
        let mut inner = self.inner.lock().expect("rate limiter poisoned");
        inner.buckets.retain(|_, bucket| {
            while let Some(front) = bucket.events.front() {
                if now.duration_since(*front) > max_age {
                    bucket.events.pop_front();
                } else {
                    break;
                }
            }
            !bucket.events.is_empty()
        });
    }

    pub fn register_allowed(&self, ip: &str) -> bool {
        self.check(
            &format!("register:{ip}"),
            self.limits.register_per_hour,
            Duration::from_secs(3600),
        )
    }

    pub fn login_allowed(&self, username: &str) -> bool {
        self.check(
            &format!("login:{username}"),
            self.limits.login_per_5min,
            Duration::from_secs(300),
        )
    }

    pub fn send_allowed(&self, user_id: &str) -> bool {
        self.check(
            &format!("send:{user_id}"),
            self.limits.send_per_minute,
            Duration::from_secs(60),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_limits_and_expires() {
        let limiter = RateLimiter::new(RateLimits {
            register_per_hour: 2,
            login_per_5min: 2,
            send_per_minute: 2,
        });
        let now = Instant::now();
        assert!(limiter.check_at("k", 2, Duration::from_secs(60), now));
        assert!(limiter.check_at("k", 2, Duration::from_secs(60), now));
        assert!(!limiter.check_at("k", 2, Duration::from_secs(60), now));
        let later = now + Duration::from_secs(61);
        assert!(limiter.check_at("k", 2, Duration::from_secs(60), later));
    }

    #[test]
    fn zero_limit_denies() {
        let limiter = RateLimiter::new(RateLimits::default());
        assert!(!limiter.check_at("k", 0, Duration::from_secs(1), Instant::now()));
    }
}
