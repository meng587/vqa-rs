//! 轻量运行现场记录（诊断包用）：
//! - `ev()`   事件时间线：错误、桥启停、断流重连、丢帧突增等，带 +秒时间戳，保留最近 50 条
//! - `sample()` 播放采样：每 5s 一条 avsync/丢帧/码率/帧率，保留最近 60 条
//!
//! 与 `diag::collect` 的分工：collect 抓"此刻的完整快照"，这里补"刚才发生过什么"。

use std::collections::VecDeque;
use std::sync::Mutex;

pub static EVENTS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
pub static SAMPLES: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

const EV_CAP: usize = 50;
const SP_CAP: usize = 60;

fn since() -> f64 {
    crate::diag::APP_START
        .get()
        .map(|t| t.elapsed().as_secs_f64())
        .unwrap_or(0.0)
}

fn push(buf: &Mutex<VecDeque<String>>, line: String, cap: usize) {
    if let Ok(mut g) = buf.lock() {
        g.push_back(line);
        while g.len() > cap {
            g.pop_front();
        }
    }
}

/// 记一条事件（自动带 +秒时间戳）。
pub fn ev(tag: &str, msg: &str) {
    push(
        &EVENTS,
        format!("[+{:7.2}s] [{tag}] {msg}", since()),
        EV_CAP,
    );
}

/// 记一条播放采样。
pub fn sample(msg: impl Into<String>) {
    push(&SAMPLES, format!("[+{:7.2}s] {}", since(), msg.into()), SP_CAP);
}

pub fn events_dump() -> String {
    dump(&EVENTS, "（本次运行还没有事件记录）")
}

pub fn samples_dump() -> String {
    dump(&SAMPLES, "（还没有播放采样）")
}

fn dump(buf: &Mutex<VecDeque<String>>, empty: &str) -> String {
    match buf.lock() {
        Ok(g) if !g.is_empty() => g.iter().cloned().collect::<Vec<_>>().join("\n"),
        _ => empty.to_string(),
    }
}

/// 新起播时清掉上一次的采样（事件保留，跨会话仍有用）。
pub fn clear_samples() {
    if let Ok(mut g) = SAMPLES.lock() {
        g.clear();
    }
}
