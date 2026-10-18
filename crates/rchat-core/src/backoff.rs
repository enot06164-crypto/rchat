//! Экспоненциальный backoff с jitter и верхним пределом.
//!
//! Используется для реконнекта и повторных попыток outbox. Постоянные ошибки
//! (авторизация, валидация) не должны повторяться как временные — это
//! решается вызывающим кодом через `CoreError::is_permanent`.

use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct BackoffConfig {
    /// Задержка первой повторной попытки.
    pub initial: Duration,
    /// Верхний предел задержки.
    pub max: Duration,
    /// Множитель роста.
    pub multiplier: f64,
    /// Доля случайности: задержка равномерно распределяется в
    /// `[base * (1 - jitter), base * (1 + jitter)]`, где `base` — теоретическая.
    pub jitter: f64,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(500),
            max: Duration::from_secs(30),
            multiplier: 2.0,
            jitter: 0.3,
        }
    }
}

/// Генератор задержек. Детерминирован по числу попыток; jitter берётся из ОС.
#[derive(Debug, Clone)]
pub struct Backoff {
    config: BackoffConfig,
    attempt: u32,
}

impl Backoff {
    pub fn new(config: BackoffConfig) -> Self {
        Self { config, attempt: 0 }
    }

    /// Сброс после успеха.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    pub fn attempts(&self) -> u32 {
        self.attempt
    }

    /// Следующая задержка; увеличивает счётчик попыток.
    pub fn next_delay(&mut self) -> Duration {
        let delay = delay_for_attempt(&self.config, self.attempt);
        self.attempt = self.attempt.saturating_add(1);
        delay
    }
}

/// Задержка для конкретной попытки (0 — первая). Детерминированная база
/// плюс случайный jitter из ОС.
pub fn delay_for_attempt(config: &BackoffConfig, attempt: u32) -> Duration {
    let base = config.initial.as_secs_f64() * config.multiplier.powi(attempt.min(16) as i32);
    let base = base.min(config.max.as_secs_f64());
    let jitter = uniform_unit() * 2.0 - 1.0; // [-1, 1]
    let factor = 1.0 + config.jitter * jitter;
    let delay = (base * factor.clamp(0.05, 2.0)).min(config.max.as_secs_f64());
    Duration::from_secs_f64(delay.max(0.001))
}

/// Равномерное [0,1) из ОС; при сбое генератора возвращает 0.5
/// (детерминированный, но безопасный запас — jitter без случайности).
fn uniform_unit() -> f64 {
    let mut buf = [0u8; 8];
    if getrandom::fill(&mut buf).is_err() {
        return 0.5;
    }
    let value = u64::from_le_bytes(buf);
    (value >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BackoffConfig {
        BackoffConfig {
            initial: Duration::from_millis(100),
            max: Duration::from_millis(800),
            multiplier: 2.0,
            jitter: 0.25,
        }
    }

    #[test]
    fn grows_and_caps() {
        let mut backoff = Backoff::new(cfg());
        for _ in 0..20 {
            let delay = backoff.next_delay();
            assert!(delay <= Duration::from_millis(800));
            assert!(delay >= Duration::from_millis(7));
        }
        // Первая попытка — около initial (±jitter).
        let mut fresh = Backoff::new(cfg());
        let first = fresh.next_delay();
        assert!(first >= Duration::from_millis(75));
        assert!(first <= Duration::from_millis(125));
    }

    #[test]
    fn reset_restarts() {
        let mut backoff = Backoff::new(cfg());
        for _ in 0..5 {
            backoff.next_delay();
        }
        backoff.reset();
        let after = backoff.next_delay();
        assert!(after <= Duration::from_millis(125));
    }
}
