//! Daemon-side rate and volume limits.
//!
//! These hold even if the server is compromised: a flood of calls or a bulk
//! read is refused locally, whatever the server's own limits say.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::Limits;
use crate::error::{ErrorCode, ToolError};

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(24 * 3600);
/// Chats tracked for per-chat budgets; the oldest are forgotten first.
const MAX_TRACKED_CHATS: usize = 1024;

#[derive(Default)]
struct Windows {
    calls: VecDeque<Instant>,
    reads: VecDeque<(Instant, usize)>,
    chat_reads: HashMap<String, VecDeque<(Instant, usize)>>,
    /// Changes made, with the bytes each wrote.
    changes: VecDeque<(Instant, u64)>,
}

pub struct Limiter {
    limits: Mutex<Limits>,
    windows: Mutex<Windows>,
}

impl Limiter {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits: Mutex::new(limits),
            windows: Mutex::default(),
        }
    }

    pub fn set_limits(&self, limits: Limits) {
        *self.limits.lock().expect("limits") = limits;
    }

    /// Count one tool call against the per-minute limit.
    pub fn check_call(&self) -> Result<(), ToolError> {
        self.check_call_at(Instant::now())
    }

    fn check_call_at(&self, now: Instant) -> Result<(), ToolError> {
        let max = self.limits.lock().expect("limits").calls_per_minute as usize;
        let mut w = self.windows.lock().expect("windows");
        prune(&mut w.calls, now, MINUTE, |t| *t);
        if w.calls.len() >= max {
            return Err(ToolError::new(
                ErrorCode::RateLimited,
                format!("more than {max} tool calls in the last minute; try again shortly"),
            ));
        }
        w.calls.push_back(now);
        Ok(())
    }

    /// How many characters a read may return right now for `chat`.
    pub fn read_allowance(&self, chat: &str) -> Result<usize, ToolError> {
        self.read_allowance_at(chat, Instant::now())
    }

    fn read_allowance_at(&self, chat: &str, now: Instant) -> Result<usize, ToolError> {
        let limits = self.limits.lock().expect("limits").clone();
        let mut w = self.windows.lock().expect("windows");
        prune(&mut w.reads, now, HOUR, |(t, _)| *t);
        let total: usize = w.reads.iter().map(|(_, n)| n).sum();
        let chat_used: usize = match w.chat_reads.get_mut(chat) {
            Some(q) => {
                prune(q, now, HOUR, |(t, _)| *t);
                q.iter().map(|(_, n)| n).sum()
            }
            None => 0,
        };
        let allowance = limits
            .read_chars_per_call
            .min(limits.read_chars_per_hour.saturating_sub(total))
            .min(limits.read_chars_per_chat_hour.saturating_sub(chat_used));
        if allowance == 0 {
            return Err(ToolError::new(
                ErrorCode::RateLimited,
                "the hourly read budget is used up; try again later",
            ));
        }
        Ok(allowance)
    }

    /// Record characters returned by a read.
    pub fn record_read(&self, chat: &str, chars: usize) {
        self.record_read_at(chat, chars, Instant::now());
    }

    fn record_read_at(&self, chat: &str, chars: usize, now: Instant) {
        if chars == 0 {
            return;
        }
        let mut w = self.windows.lock().expect("windows");
        w.reads.push_back((now, chars));
        if !w.chat_reads.contains_key(chat) && w.chat_reads.len() >= MAX_TRACKED_CHATS {
            w.chat_reads
                .retain(|_, q| q.back().is_some_and(|(t, _)| now.duration_since(*t) < HOUR));
        }
        w.chat_reads
            .entry(chat.to_string())
            .or_default()
            .push_back((now, chars));
    }
}

impl Limiter {
    /// Count one change that will write `bytes`, against the change budgets
    /// (separate from the read budgets). Refused when any is used up.
    pub fn check_change(&self, bytes: u64) -> Result<(), ToolError> {
        self.check_change_at(bytes, Instant::now())
    }

    fn check_change_at(&self, bytes: u64, now: Instant) -> Result<(), ToolError> {
        let limits = self.limits.lock().expect("limits").clone();
        let mut w = self.windows.lock().expect("windows");
        prune(&mut w.changes, now, DAY, |(t, _)| *t);
        let within = |window: Duration| {
            w.changes
                .iter()
                .filter(move |(t, _)| now.duration_since(*t) < window)
        };
        let last_minute = within(MINUTE).count();
        let last_day = w.changes.len();
        let last_hour_bytes: u64 = within(HOUR).map(|(_, b)| b).sum();
        if last_minute >= limits.changes_per_minute as usize {
            return Err(ToolError::new(
                ErrorCode::RateLimited,
                format!(
                    "This computer allows {} changes a minute. Try again shortly.",
                    limits.changes_per_minute
                ),
            ));
        }
        if last_day >= limits.changes_per_day as usize {
            return Err(ToolError::new(
                ErrorCode::RateLimited,
                format!(
                    "This computer allows {} changes a day, and they are used up. Try again \
                     later.",
                    limits.changes_per_day
                ),
            ));
        }
        if last_hour_bytes.saturating_add(bytes) > limits.change_bytes_per_hour {
            return Err(ToolError::new(
                ErrorCode::RateLimited,
                format!(
                    "This computer allows changes to write {} MB an hour, and this one would go \
                     over. Try again later.",
                    limits.change_bytes_per_hour / (1024 * 1024)
                ),
            ));
        }
        w.changes.push_back((now, bytes));
        Ok(())
    }
}

fn prune<T>(q: &mut VecDeque<T>, now: Instant, window: Duration, at: impl Fn(&T) -> Instant) {
    while q
        .front()
        .is_some_and(|x| now.duration_since(at(x)) >= window)
    {
        q.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            calls_per_minute: 3,
            read_chars_per_call: 100,
            read_chars_per_chat_hour: 250,
            read_chars_per_hour: 400,
            ..Limits::default()
        }
    }

    #[test]
    fn limits_calls_per_minute() {
        let l = Limiter::new(limits());
        let t0 = Instant::now();
        for _ in 0..3 {
            l.check_call_at(t0).unwrap();
        }
        assert_eq!(
            l.check_call_at(t0).unwrap_err().code,
            ErrorCode::RateLimited
        );
        l.check_call_at(t0 + MINUTE).unwrap();
    }

    #[test]
    fn budgets_changes_apart_from_reads() {
        let l = Limiter::new(Limits {
            changes_per_minute: 2,
            changes_per_day: 3,
            change_bytes_per_hour: 100,
            ..limits()
        });
        let t0 = Instant::now();
        l.check_change_at(10, t0).unwrap();
        l.check_change_at(10, t0).unwrap();
        let err = l.check_change_at(10, t0).unwrap_err();
        assert_eq!(err.code, ErrorCode::RateLimited);
        assert!(
            err.message.contains("2 changes a minute"),
            "{}",
            err.message
        );
        // Too many bytes for the hour.
        let err = l.check_change_at(81, t0 + MINUTE).unwrap_err();
        assert!(err.message.contains("an hour"), "{}", err.message);
        l.check_change_at(80, t0 + MINUTE).unwrap();
        // Three a day.
        let err = l.check_change_at(0, t0 + HOUR).unwrap_err();
        assert!(err.message.contains("3 changes a day"), "{}", err.message);
        l.check_change_at(0, t0 + DAY).unwrap();
        // Reads are a separate budget.
        assert_eq!(l.read_allowance_at("a", t0).unwrap(), 100);
    }

    #[test]
    fn budgets_reads_per_chat_and_total() {
        let l = Limiter::new(limits());
        let t0 = Instant::now();
        assert_eq!(l.read_allowance_at("a", t0).unwrap(), 100);
        l.record_read_at("a", 100, t0);
        l.record_read_at("a", 100, t0);
        assert_eq!(l.read_allowance_at("a", t0).unwrap(), 50);
        l.record_read_at("a", 50, t0);
        assert_eq!(
            l.read_allowance_at("a", t0).unwrap_err().code,
            ErrorCode::RateLimited
        );
        // Another chat still has room, up to the global budget.
        assert_eq!(l.read_allowance_at("b", t0).unwrap(), 100);
        l.record_read_at("b", 100, t0);
        assert_eq!(l.read_allowance_at("b", t0).unwrap(), 50);
        // An hour later everything is free again.
        assert_eq!(l.read_allowance_at("a", t0 + HOUR).unwrap(), 100);
    }
}
