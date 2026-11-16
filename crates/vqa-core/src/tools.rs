//! 工具链定位 —— 全应用**唯一**的二进制查找入口。
//!
//! 合并三处来源的口径：
//! - Python `vkitcore/media/ffmpeg.py`：ffmpeg/ffprobe 配对查找、用户手填路径、PATH
//! - Python `dev/bin`：ffmpeg.exe / ffprobe.exe / davs2-10bit / libmpv-2.dll
//! - Rust `vkitplayer/src/tools.rs`：mpv.exe 定位（本包 `bin\mpv\` 或 `D:\vkitplayer\bin\mpv\`）
//!
//! 查找顺序（ffmpeg）：
//! 1. 用户在共享配置里手填的 `ffmpeg_path`
//! 2. 环境变量 `VQA_FFMPEG`
//! 3. exe 同级 / 上级 / `bin\` 子目录（绿色部署）
//! 4. 旁挂的开发树 `dev\bin\`（开发期复用 D:\音视频工具\dev\bin）
//! 5. 系统 PATH
//!
//! ffprobe **配对**：优先取 ffmpeg 同目录（同一份构建），再走自身路径链。

use std::path::{Path, PathBuf};

const FFMPEG: &str = "ffmpeg.exe";
const FFPROBE: &str = "ffprobe.exe";

fn is_file(p: &Path) -> bool {
    p.is_file()
}

/// 某一层目录下可能的开发树位置：`<base>\dev`，以及旧布局 `<base>\音视频工具\dev`。
fn dev_at(base: &Path) -> Option<PathBuf> {
    for rel in [&["dev"][..], &["音视频工具", "dev"][..]] {
        let p = rel.iter().fold(base.to_path_buf(), |acc, s| acc.join(s));
        if p.join("vkitcore").is_dir() {
            return Some(p);
        }
    }
    None
}

/// 旁挂的 Python 开发树根（含 `dev\vkitcore` 的目录）。
///
/// 查找顺序：环境变量 `VQA_DEV` → 共享配置 `dev_root` → exe 祖先逐级上溯（≤8 级）
/// → 清单目录祖先上溯（≤5 级）。
///
/// 历史教训：早期实现假设"清单目录上溯三级即 `D:\音视频工具`"，仓库搬到
/// `D:\vqa-rs` 后该假设失效，`davs2` 定位直接返回 None（AVS2 播放整链失效，
/// 单测 `dev_root_finds_dev_tree` 报警）。故改为逐级上溯 + 可配置，不再绑定层级。
pub fn dev_root() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("VQA_DEV") {
        let p = PathBuf::from(v);
        if p.join("vkitcore").is_dir() {
            return Some(p);
        }
    }
    // 安装后与开发树分离部署时，手填一次共享配置即可，无需设环境变量
    if let Some(p) = crate::config::shared_get("dev_root")
        .and_then(|v| v.as_str().map(PathBuf::from))
        .filter(|p| p.join("vkitcore").is_dir())
    {
        return Some(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        // exe 所在目录起逐级上溯（target\debug\deps → … → 盘根）
        let mut d = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..8 {
            let Some(cur) = d else { break };
            if let Some(hit) = dev_at(&cur) {
                return Some(hit);
            }
            d = cur.parent().map(|p| p.to_path_buf());
        }
    }
    let mut d = Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    for _ in 0..5 {
        let Some(cur) = d else { break };
        if let Some(hit) = dev_at(&cur) {
            return Some(hit);
        }
        d = cur.parent().map(|p| p.to_path_buf());
    }
    None
}

fn dev_bin(rel: &[&str]) -> Option<PathBuf> {
    let root = dev_root()?;
    let p = rel.iter().fold(root, |acc, s| acc.join(s));
    is_file(&p).then_some(p)
}

fn find_in_dirs(name: &str) -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent().map(|p| p.to_path_buf());
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = &exe_dir {
        dirs.push(d.clone());
        if let Some(up) = d.parent() {
            dirs.push(up.to_path_buf());
        }
    }
    // 开发态：清单目录也算一处（cargo run 时 exe 在 target\ 下）
    dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    if let Some(up) = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.to_path_buf())
    {
        dirs.push(up);
    }
    dirs.into_iter()
        .flat_map(|d| [d.join(name), d.join("bin").join(name)])
        .find(|p| is_file(p))
}

/// 定位 ffmpeg。找不到返回 None。
pub fn find_ffmpeg() -> Option<PathBuf> {
    // 1) 用户手填（共享配置，键名沿用 Python 版 ffmpeg_path）
    if let Some(v) = crate::config::shared_get("ffmpeg_path") {
        if let Some(s) = v.as_str() {
            let p = PathBuf::from(s);
            if is_file(&p) {
                return Some(p);
            }
        }
    }
    // 2) 环境变量
    if let Ok(p) = std::env::var("VQA_FFMPEG") {
        let p = PathBuf::from(p);
        if is_file(&p) {
            return Some(p);
        }
    }
    // 3) 程序同级
    if let Some(p) = find_in_dirs(FFMPEG) {
        return Some(p);
    }
    // 4) 开发树
    if let Some(p) = dev_bin(&["bin", FFMPEG]) {
        return Some(p);
    }
    // 5) PATH
    which(FFMPEG)
}

/// 定位 ffprobe：先取 ffmpeg 同目录（配对），再走自身查找链。
pub fn find_ffprobe() -> Option<PathBuf> {
    if let Some(ffm) = find_ffmpeg() {
        let p = ffm.parent().unwrap_or(Path::new(".")).join(FFPROBE);
        if is_file(&p) {
            return Some(p);
        }
    }
    if let Some(p) = find_in_dirs(FFPROBE) {
        return Some(p);
    }
    if let Some(p) = dev_bin(&["bin", FFPROBE]) {
        return Some(p);
    }
    which(FFPROBE)
}

/// 定位 mpv.exe：vqa-rs 旁的 `bin\mpv\mpv.exe` → `D:\vkitplayer\bin\mpv\mpv.exe`。
pub fn find_mpv() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VQA_MPV") {
        let p = PathBuf::from(p);
        if is_file(&p) {
            return Some(p);
        }
    }
    if let Some(p) = find_in_dirs("mpv.exe") {
        return Some(p);
    }
    // 相对清单：vqa-rs\bin\mpv\mpv.exe
    let cands = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bin/mpv/mpv.exe"),
        PathBuf::from(r"D:\vkitplayer\bin\mpv\mpv.exe"),
    ];
    if let Some(p) = cands.into_iter().find(|p| is_file(p)) {
        return Some(p);
    }
    dev_bin(&["mpv", "mpv.exe"])
}

/// 自编译的管道版 10-bit AVS2 解码器（dev\bin\davs2-10bit\vkit_davs2.exe）。
///
/// 便携部署可直接把 `vkit_davs2.exe` 放在 exe 同级或 `bin\` 下，不再依赖开发树。
pub fn find_davs2() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VQA_DAVS2") {
        let p = PathBuf::from(p);
        if is_file(&p) {
            return Some(p);
        }
    }
    if let Some(p) = find_in_dirs("vkit_davs2.exe") {
        return Some(p);
    }
    dev_bin(&["bin", "davs2-10bit", "vkit_davs2.exe"])
}

/// 极简 which（Windows：补 .exe 后缀再试）。
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exe_names: Vec<String> = if name.to_ascii_lowercase().ends_with(".exe") {
        vec![name.to_string()]
    } else {
        vec![format!("{name}.exe"), name.to_string()]
    };
    for dir in std::env::split_paths(&path) {
        for n in &exe_names {
            let p = dir.join(n);
            if is_file(&p) {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_root_finds_dev_tree() {
        // 本机挂着开发树时，定位链必须能找到；没挂（CI/纯发布机）则跳过。
        // 注意：守卫不能写死某个绝对路径——早前写成 `D:\音视频工具\dev`，
        // 仓库搬走后守卫仍为真、断言却失败，制造了假警报。
        match dev_root() {
            None => {}
            Some(root) => {
                assert!(root.join("vkitcore").is_dir(), "dev_root 必须含 vkitcore");
                assert!(find_ffmpeg().is_some(), "应能找到 ffmpeg");
                assert!(find_ffprobe().is_some(), "应能找到 ffprobe");
                if root.join("bin/davs2-10bit/vkit_davs2.exe").is_file() {
                    assert!(find_davs2().is_some(), "开发树有 davs2 时应能定位");
                }
            }
        }
    }
}
