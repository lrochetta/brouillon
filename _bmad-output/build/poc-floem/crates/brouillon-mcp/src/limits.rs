//! Per-client rate limit, in-flight cap and circuit breaker (spec 60 sections 3.5 and BK-MCP-019).
//!
//! Pure and clock-injected (`now_ms` is passed in), so it is tested without sleeping.
//! * calls: token bucket, `rate_per_s` sustained with `burst` capacity;
//! * writes (`edit`, `save`, `annotate`, `run_command`): a second, smaller bucket;
//! * in flight: at most `max_in_flight` calls running at once per client;
//! * breaker: `breaker_threshold` refused calls in a row pause the client for `breaker_pause_ms`.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    pub rate_per_s: f64,
    pub burst: f64,
    pub write_per_s: f64,
    pub write_burst: f64,
    pub max_in_flight: usize,
    pub breaker_threshold: u32,
    pub breaker_pause_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { rate_per_s: 10.0, burst: 30.0, write_per_s: 2.0, write_burst: 6.0, max_in_flight: 4, breaker_threshold: 8, breaker_pause_ms: 30_000 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitError {
    /// Too many calls too fast; retry after `retry_ms`.
    Rate { retry_ms: u64 },
    /// Too many calls running at once.
    InFlight,
    /// The circuit breaker is open.
    Paused { retry_ms: u64 },
}

struct State {
    tokens: f64,
    write_tokens: f64,
    last_ms: u64,
    in_flight: usize,
    denials: u32,
    paused_until: u64,
}

#[derive(Default)]
pub struct Limiter {
    cfg: Limits,
    clients: HashMap<String, State>,
}

impl Limiter {
    pub fn new(cfg: Limits) -> Self {
        Self { cfg, clients: HashMap::new() }
    }

    fn state(&mut self, client: &str, now_ms: u64) -> &mut State {
        let (b, wb) = (self.cfg.burst, self.cfg.write_burst);
        self.clients.entry(client.to_string()).or_insert(State { tokens: b, write_tokens: wb, last_ms: now_ms, in_flight: 0, denials: 0, paused_until: 0 })
    }

    /// Admit one call or say why not. Every `Ok` must be paired with [`Limiter::end`].
    pub fn begin(&mut self, client: &str, write: bool, now_ms: u64) -> Result<(), LimitError> {
        let cfg = self.cfg.clone();
        let st = self.state(client, now_ms);
        if st.paused_until > now_ms {
            return Err(LimitError::Paused { retry_ms: st.paused_until - now_ms });
        }
        let dt = now_ms.saturating_sub(st.last_ms) as f64 / 1000.0;
        st.last_ms = now_ms;
        st.tokens = (st.tokens + dt * cfg.rate_per_s).min(cfg.burst);
        st.write_tokens = (st.write_tokens + dt * cfg.write_per_s).min(cfg.write_burst);
        if st.in_flight >= cfg.max_in_flight {
            return Err(LimitError::InFlight);
        }
        if st.tokens < 1.0 {
            return Err(LimitError::Rate { retry_ms: ((1.0 - st.tokens) / cfg.rate_per_s * 1000.0).ceil() as u64 });
        }
        if write && st.write_tokens < 1.0 {
            return Err(LimitError::Rate { retry_ms: ((1.0 - st.write_tokens) / cfg.write_per_s * 1000.0).ceil() as u64 });
        }
        st.tokens -= 1.0;
        if write {
            st.write_tokens -= 1.0;
        }
        st.in_flight += 1;
        Ok(())
    }

    /// The call finished. `refused` = the policy said no (consent, scope, denylist). Returns `true`
    /// when this refusal just opened the circuit breaker.
    pub fn end(&mut self, client: &str, refused: bool, now_ms: u64) -> bool {
        let cfg = self.cfg.clone();
        let st = self.state(client, now_ms);
        st.in_flight = st.in_flight.saturating_sub(1);
        if !refused {
            st.denials = 0;
            return false;
        }
        st.denials += 1;
        if st.denials >= cfg.breaker_threshold {
            st.denials = 0;
            st.paused_until = now_ms + cfg.breaker_pause_ms;
            return true;
        }
        false
    }

    /// The user answered a consent prompt (or reset the client): forget its history.
    pub fn reset(&mut self, client: &str) {
        self.clients.remove(client);
    }
}

/// Auto-off rule of `[mcp] auto_off_minutes`: only with no client connected and idle long enough.
pub fn auto_off_due(minutes: Option<u32>, connected: usize, idle: std::time::Duration) -> bool {
    match minutes {
        Some(m) if m > 0 => connected == 0 && idle >= std::time::Duration::from_secs(m as u64 * 60),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn burst_then_sustained_rate() {
        let mut l = Limiter::new(Limits::default());
        for _ in 0..30 {
            l.begin("c", false, 0).unwrap();
            l.end("c", false, 0);
        }
        assert!(matches!(l.begin("c", false, 0), Err(LimitError::Rate { .. })), "31st call in the same instant");
        // 100 ms later one token (10/s) is back, not two.
        l.begin("c", false, 100).unwrap();
        l.end("c", false, 100);
        assert!(l.begin("c", false, 100).is_err());
        // Another client is unaffected.
        l.begin("other", false, 100).unwrap();
    }

    #[test]
    fn writes_are_limited_harder() {
        let mut l = Limiter::new(Limits::default());
        let mut ok = 0;
        for _ in 0..20 {
            if l.begin("c", true, 0).is_ok() {
                ok += 1;
                l.end("c", false, 0);
            }
        }
        assert_eq!(ok, 6, "write burst");
        assert!(l.begin("c", false, 0).is_ok(), "reads still pass");
        l.end("c", false, 0);
        assert!(l.begin("c", true, 600).is_ok(), "2 writes/s: one is back after 500 ms");
    }

    #[test]
    fn in_flight_cap() {
        let mut l = Limiter::new(Limits::default());
        for _ in 0..4 {
            l.begin("c", false, 0).unwrap();
        }
        assert_eq!(l.begin("c", false, 0), Err(LimitError::InFlight));
        l.end("c", false, 0);
        assert!(l.begin("c", false, 0).is_ok());
    }

    #[test]
    fn breaker_opens_after_n_refusals_and_closes_after_the_pause() {
        let mut l = Limiter::new(Limits::default());
        let mut opened = false;
        for i in 0..8 {
            l.begin("c", false, i).unwrap();
            opened = l.end("c", true, i);
        }
        assert!(opened, "the 8th consecutive refusal opens it");
        match l.begin("c", false, 100) {
            Err(LimitError::Paused { retry_ms }) => assert!(retry_ms > 29_000),
            other => panic!("{other:?}"),
        }
        assert!(l.begin("c", false, 30_100).is_ok(), "closed after 30 s");
    }

    #[test]
    fn a_success_resets_the_streak() {
        let mut l = Limiter::new(Limits::default());
        for i in 0..7 {
            l.begin("c", false, i).unwrap();
            l.end("c", true, i);
        }
        l.begin("c", false, 8).unwrap();
        l.end("c", false, 8);
        for i in 9..16 {
            l.begin("c", false, i).unwrap();
            assert!(!l.end("c", true, i));
        }
    }

    #[test]
    fn auto_off_rule() {
        let m = Duration::from_secs(60);
        assert!(!auto_off_due(None, 0, m * 999));
        assert!(!auto_off_due(Some(0), 0, m * 999));
        assert!(!auto_off_due(Some(10), 1, m * 999), "a connected client keeps it on");
        assert!(!auto_off_due(Some(10), 0, m * 9));
        assert!(auto_off_due(Some(10), 0, m * 10));
    }
}
