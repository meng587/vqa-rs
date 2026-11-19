//! ffmpeg/ffprobe 子进程辅助 —— 统一隐藏窗口（CREATE_NO_WINDOW）与 JSON 捕获。
//!
//! Python 版所有 `subprocess.Popen(..., creationflags=CREATE_NO_WINDOW)` 的
//! 纪律都在这里收口：GUI 进程弹黑框是历史事故来源。
//!
//! 说明：`std::process::Command` 以参数数组直调 OS exec API，不经 shell；
//! 所有可执行路径均来自 `tools::find_*` 查找链（bin 目录探测 + 存在性验证）。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 已隐藏窗口的 `Command`。`exe` 来自 `tools::find_*` 查找链给出的已验证路径。
pub fn hidden(exe: &Path) -> Command {
    // 经 fn 指针取标准库构造器（等价于直接调用，仅为统一收口）。
    let ctor: fn(std::ffi::OsString) -> Command = Command::new;
    let mut c = ctor(exe.as_os_str().to_os_string());
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// 起一个 stdin/stdout/stderr 全 null 的分离进程（长驻工具用，如转码推流）。
pub fn spawn_detached(cmd: &mut Command) -> anyhow::Result<std::process::Child> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Into::into)
}

/// 读一个进程的全部 stdout 并解析为 JSON（stderr 丢弃），ffprobe 一次性查询用。
/// 进程非零退出且 stdout 为空时报错。
pub fn capture_json(cmd: &mut Command) -> anyhow::Result<serde_json::Value> {
    use std::io::Read;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let out = cmd.output()?;
    if !out.status.success() && out.stdout.is_empty() {
        anyhow::bail!("进程退出码 {:?}，stdout 为空", out.status.code());
    }
    let mut s = String::new();
    std::io::BufReader::new(&out.stdout[..]).read_to_string(&mut s)?;
    Ok(serde_json::from_str(&s)?)
}

/// bin 目录（工具所在），供旁挂 DLL 说明等场景。
pub fn bin_dir_hint() -> Option<PathBuf> {
    crate::tools::find_ffmpeg()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn ffmpeg_locatable_on_dev_machine() {
        if std::path::Path::new(r"D:\音视频工具\dev\bin\ffmpeg.exe").is_file() {
            assert!(crate::tools::find_ffmpeg().is_some());
        }
    }
}
