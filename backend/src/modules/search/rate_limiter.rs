//! Ограничение частоты запросов к аптекам.
//!
//! PHARMACY_REQUEST_INTERVAL_SECS — будет уменьшаться по мере стабилизации системы.
//!
//! Лимит глобальный: учитываются запросы из ВСЕХ поисков к одной торговой точке.
//! Хранится в памяти процесса (модульный монолит, один экземпляр). При
//! горизонтальном масштабировании заменить на Redis/PostgreSQL с тем же интерфейсом.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

pub struct PharmacyRateLimiter {
    interval: Duration,
    last_request: Mutex<HashMap<Uuid, Instant>>,
}

impl PharmacyRateLimiter {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_request: Mutex::new(HashMap::new()),
        }
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Пытается «занять» слот для аптеки. `Err(wait)` — сколько ещё ждать.
    pub fn try_acquire(&self, pharmacy_key: Uuid) -> Result<(), Duration> {
        self.try_acquire_at(pharmacy_key, Instant::now())
    }

    pub fn try_acquire_at(&self, pharmacy_key: Uuid, now: Instant) -> Result<(), Duration> {
        if self.interval.is_zero() {
            return Ok(());
        }
        let mut map = self.last_request.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(prev) = map.get(&pharmacy_key) {
            let elapsed = now.saturating_duration_since(*prev);
            if elapsed < self.interval {
                return Err(self.interval - elapsed);
            }
        }
        map.insert(pharmacy_key, now);
        Ok(())
    }

    /// Ждёт, пока слот освободится (но не дольше `max_wait`). `false` — не дождались.
    pub async fn acquire(&self, pharmacy_key: Uuid, max_wait: Duration) -> bool {
        let deadline = Instant::now() + max_wait;
        loop {
            match self.try_acquire(pharmacy_key) {
                Ok(()) => return true,
                Err(wait) => {
                    let now = Instant::now();
                    if now + wait > deadline {
                        return false;
                    }
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    /// Очистка устаревших записей (вызывается фоновой задачей обслуживания).
    pub fn cleanup(&self) {
        let now = Instant::now();
        let mut map = self.last_request.lock().unwrap_or_else(|p| p.into_inner());
        map.retain(|_, t| now.saturating_duration_since(*t) < self.interval);
    }

    pub fn tracked(&self) -> usize {
        self.last_request.lock().map(|m| m.len()).unwrap_or(0)
    }
}

/// Простой лимитер API-запросов по ключу (IP) с фиксированным окном в 1 минуту —
/// защита от массовых запросов.
pub struct ApiRateLimiter {
    per_minute: u32,
    windows: Mutex<HashMap<String, (Instant, u32)>>,
}

impl ApiRateLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            windows: Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, key: &str) -> Result<(), u64> {
        self.check_at(key, Instant::now())
    }

    pub fn check_at(&self, key: &str, now: Instant) -> Result<(), u64> {
        let window = Duration::from_secs(60);
        let mut map = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() > 100_000 {
            map.retain(|_, (start, _)| now.saturating_duration_since(*start) < window);
        }
        let entry = map.entry(key.to_string()).or_insert((now, 0));
        if now.saturating_duration_since(entry.0) >= window {
            *entry = (now, 0);
        }
        if entry.1 >= self.per_minute {
            let retry = window.saturating_sub(now.saturating_duration_since(entry.0));
            return Err(retry.as_secs().max(1));
        }
        entry.1 += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_request_within_interval_is_blocked() {
        let l = PharmacyRateLimiter::new(Duration::from_secs(60));
        let p = Uuid::new_v4();
        let t0 = Instant::now();
        assert!(l.try_acquire_at(p, t0).is_ok());
        let wait = l.try_acquire_at(p, t0 + Duration::from_secs(20)).unwrap_err();
        assert_eq!(wait, Duration::from_secs(40));
        assert!(l.try_acquire_at(p, t0 + Duration::from_secs(60)).is_ok());
    }

    #[test]
    fn limit_is_per_pharmacy() {
        let l = PharmacyRateLimiter::new(Duration::from_secs(60));
        let t0 = Instant::now();
        assert!(l.try_acquire_at(Uuid::new_v4(), t0).is_ok());
        assert!(l.try_acquire_at(Uuid::new_v4(), t0).is_ok());
    }

    #[test]
    fn zero_interval_disables_limit() {
        let l = PharmacyRateLimiter::new(Duration::ZERO);
        let p = Uuid::new_v4();
        assert!(l.try_acquire(p).is_ok());
        assert!(l.try_acquire(p).is_ok());
    }

    #[tokio::test]
    async fn acquire_respects_max_wait() {
        let l = PharmacyRateLimiter::new(Duration::from_secs(60));
        let p = Uuid::new_v4();
        assert!(l.acquire(p, Duration::ZERO).await);
        assert!(!l.acquire(p, Duration::from_millis(10)).await);
    }

    #[tokio::test]
    async fn acquire_waits_for_slot() {
        let l = PharmacyRateLimiter::new(Duration::from_millis(50));
        let p = Uuid::new_v4();
        assert!(l.acquire(p, Duration::ZERO).await);
        let start = Instant::now();
        assert!(l.acquire(p, Duration::from_secs(1)).await);
        assert!(start.elapsed() >= Duration::from_millis(40));
    }

    #[test]
    fn cleanup_removes_expired() {
        let l = PharmacyRateLimiter::new(Duration::from_millis(1));
        l.try_acquire(Uuid::new_v4()).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        l.cleanup();
        assert_eq!(l.tracked(), 0);
    }

    #[test]
    fn concurrent_acquire_allows_exactly_one() {
        use std::sync::Arc;
        // Ровно один из параллельных поисков получает слот к аптеке.
        let l = Arc::new(PharmacyRateLimiter::new(Duration::from_secs(60)));
        let p = Uuid::new_v4();
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let l = l.clone();
                std::thread::spawn(move || l.try_acquire(p).is_ok())
            })
            .collect();
        let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| **r).count(), 1);
    }

    #[test]
    fn api_limiter_blocks_after_quota() {
        let l = ApiRateLimiter::new(3);
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(l.check_at("1.2.3.4", t0).is_ok());
        }
        assert!(l.check_at("1.2.3.4", t0).is_err());
        assert!(l.check_at("5.6.7.8", t0).is_ok());
        assert!(l.check_at("1.2.3.4", t0 + Duration::from_secs(61)).is_ok());
    }
}
