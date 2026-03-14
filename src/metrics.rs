use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::models::MetricsResponse;

#[derive(Debug, serde::Serialize, Clone)]
pub struct RequestLog {
    pub segment: String,
    pub num_offers: usize,
    pub num_fields: usize,
    pub elapsed_ms: u64,
    pub gemini_calls: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub timestamp: String,
}

pub struct Metrics {
    pub gemini_request_count: AtomicU64,
    pub prompt_tokens: AtomicU64,
    pub completion_tokens: AtomicU64,
    pub total_tokens: AtomicU64,
    pub request_logs: Mutex<Vec<RequestLog>>,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            gemini_request_count: AtomicU64::new(0),
            prompt_tokens: AtomicU64::new(0),
            completion_tokens: AtomicU64::new(0),
            total_tokens: AtomicU64::new(0),
            request_logs: Mutex::new(Vec::new()),
        }
    }

    pub fn get(&self) -> MetricsResponse {
        MetricsResponse {
            gemini_request_count: self.gemini_request_count.load(Ordering::Relaxed),
            prompt_tokens: self.prompt_tokens.load(Ordering::Relaxed),
            completion_tokens: self.completion_tokens.load(Ordering::Relaxed),
            total_tokens: self.total_tokens.load(Ordering::Relaxed),
        }
    }

    pub fn reset(&self) {
        self.gemini_request_count.store(0, Ordering::Relaxed);
        self.prompt_tokens.store(0, Ordering::Relaxed);
        self.completion_tokens.store(0, Ordering::Relaxed);
        self.total_tokens.store(0, Ordering::Relaxed);
        self.request_logs.lock().unwrap().clear();
    }

    pub fn add(&self, prompt: u64, completion: u64) {
        self.gemini_request_count.fetch_add(1, Ordering::Relaxed);
        self.prompt_tokens.fetch_add(prompt, Ordering::Relaxed);
        self.completion_tokens.fetch_add(completion, Ordering::Relaxed);
        self.total_tokens
            .fetch_add(prompt + completion, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.gemini_request_count.load(Ordering::Relaxed),
            self.prompt_tokens.load(Ordering::Relaxed),
            self.completion_tokens.load(Ordering::Relaxed),
            self.total_tokens.load(Ordering::Relaxed),
        )
    }

    pub fn log_request(&self, log: RequestLog) {
        self.request_logs.lock().unwrap().push(log);
    }

    pub fn get_logs(&self) -> Vec<RequestLog> {
        self.request_logs.lock().unwrap().clone()
    }
}
