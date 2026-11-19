//! vqa-core —— 视听质量检测工作站核心库（Rust 版）。
//!
//! 从 Python 版 `vkitcore`（含 `media/*`）逐模块移植；纯逻辑、无 GUI 依赖，
//! 可 headless 测试。GUI 在 `vqa-app`。
//!
//! 模块对照：
//! | Rust            | Python 原始位置                  |
//! |-----------------|----------------------------------|
//! | `config`        | `vkitcore/config.py`             |
//! | `ffmpeg`        | `vkitcore/media/ffmpeg.py`       |
//! | `tools`         | `vkitplayer/src/tools.rs`        |
//! | `probe`         | `vkitcore/media/probe.py`        |
//! | `netif`         | `vkitcore/media/netif.py`        |
//! | `tsfile`        | `vkitcore/media/tsfile.py`       |
//! | `live`          | `AnalysisPage._live_recv_loop`   |
//! | `player`        | `vkitplayer/src/mpv.rs`          |
//! | `bridge`        | `vkitplayer/src/bridge.rs` + `media/bridge_ffmpeg.py` |

pub mod avsync;
pub mod bridge;
pub mod bridge_par;
pub mod burst;
pub mod compliance;
pub mod consist;
pub mod config;
pub mod ffmpeg;
pub mod live;
pub mod mdi;
pub mod netif;
pub mod player;
pub mod probe;
pub mod tools;
pub mod tsfile;
pub mod ts_events;
pub mod xcode;
