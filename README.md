# 视听质量检测工作站（Rust 版 / vqa-rs）

原 Python 版（`D:\音视频工具\dev`，约 2.3 万行 PyQt6）的 Rust 重写。
第一期交付：**工作区骨架 + vqa-core 核心库 + 播放器页 + 视频分析页**。

## 构建与运行

```bash
cargo build -p vqa-app            # GUI（debug）
cargo run -p vqa-app              # 启动工作站
cargo build --release -p vqa-app  # 发布构建（thin LTO）
```

无外部依赖安装：工具链（ffmpeg/ffprobe/davs2/mpv）按以下顺序定位——

1. 共享配置 `ffmpeg_path`（设置页手填，沿用 Python 版键名）
2. 环境变量 `VQA_FFMPEG` / `VQA_MPV` / `VQA_DAVS2`
3. exe 同级 / 上级 / `bin\` 子目录
4. 旁挂开发树 `D:\音视频工具\dev\bin\`（开发期自动复用，含 ffmpeg/ffprobe/davs2-10bit）
5. mpv：`D:\vkitplayer\bin\mpv\mpv.exe`
6. 系统 PATH

数据目录：`data_dir.txt` 指针文件 → `VQA_HOME` → `%APPDATA%\vqa`（同 Python 版口径）。

## 工作区结构

```
crates/vqa-core   纯逻辑核心库（无 GUI 依赖，可 headless 测试）
├── config        数据目录/配置/日志        ← vkitcore/config.py
├── tools         ffmpeg/mpv/davs2 定位     ← media/ffmpeg.py + vkitplayer/tools.rs
├── ffmpeg        子进程辅助(CREATE_NO_WINDOW/限时收割)
├── probe         ffprobe 完整/快速探测 + LRU 缓存 ← media/probe.py
├── netif         网卡枚举(iphlpapi) + bind_args ← media/netif.py
├── tsfile        TS 解析/PSI·SI/TR101290/PCR-PLL 抖动/码率 ← media/tsfile.py
├── live          UDP/RTP 实时收流(IGMP join/RTP 剥离) ← AnalysisPage
├── player        mpv.exe 命名管道 IPC 内核  ← vkitplayer/mpv.rs
├── bridge        AVS 转码桥(单级/chain10)   ← bridge_ffmpeg.py + vkitplayer/bridge.rs
crates/vqa-app    eframe/egui GUI
├── app           侧边导航(状态灯)/主题/单实例
├── pages/player  播放器页(mpv 嵌入/控制条/快照/录制/AVS 桥自动路由)
├── pages/analysis 视频分析页(7 页签 + 实时模式 + Markdown 报告)
├── video_child   mpv --wid 嵌入子窗(SetWindowPos 客户区口径)
```

## 验证工具

```bash
cargo test -p vqa-core                     # 26 个单测（CRC 向量/PSI/PLL/TR290/RTP…）
cargo run -p vqa-core --example scan -- <ts文件> [--json]   # headless 扫描（与 Python 版对拍一致）
cargo run -p vqa-core --example mpv_smoke -- [媒体文件]     # mpv IPC 全链路冒烟
```

### 对拍结论（2026-11-14）

对真实录制 TS（`录制_20260913_165127.ts`，12804 包 / 19.9s / 0.96Mbps）：

| 指标 | Python | Rust |
|---|---|---|
| 包数 / 包长 / 同步错误 | 12804 / 188 / 0 | 一致 |
| PCR 时长 / 码率点 | 19.9s / 201 | 一致 |
| 平均码率 | 958723 bps | 958724（末位舍入差）|
| 节目表 / 流类型 | PMT 0x1000, H.264+AAC | 一致 |
| TR290 | PCR_error×199, NIT_id×1, EIT_id×1 | 逐项一致 |
| CBR 判定 | false | false |

## 后续路线（未实施）

- **二期**：音画同步检测（detector：语音带包络/互相关）、一致性比对（pHash/DTW/台标）、报告增强
- **三期**：合规性检测（MDI-DF/MLR + 解码级六项）、微突发检测、直播转码页
- **四期**：频道列表全功能（分组/配对/导入导出）、批量轮巡、TTS 语音报警、设置/关于/更新器

## 备注

- 所有子进程均 `CREATE_NO_WINDOW`；stderr 一律排空（历史事故纪律，见各模块注释）。
- mpv IPC **禁止 File::flush**（FlushFileBuffers 阻塞）；每帧排空管道是保活必需。
- 本仓库由 `cargo fix` 初始化过 git（仅骨架提交未做）；如需入库请自行 commit。

## Tauri/Web UI 版（crates/vqa-tauri，当前主线）

egui 版（crates/vqa-app，保留可用）的 UI 层换成 Tauri 2 + Vite/原生 TS 后的版本；
`vqa-core` 原样复用。设计稿即成品（zinc 层叠 + 靛紫渐变，见 `ui/index.html`）。

```bash
# 开发（需 Node ≥18；WebView2 Win10/11 自带）
cd crates/vqa-tauri && npx --prefix ui tauri dev

# 发布（NSIS 安装包 + updater 签名产物）
TAURI_SIGNING_PRIVATE_KEY=keys/tauri.key npx --prefix ui tauri build
# 产物： target/release/bundle/nsis/*-setup.exe（+ .sig）
```

### 在线更新

- `tauri-plugin-updater`：检查 `update/latest.json`（Gitee raw，双路径兼容旧版
  endpoint）→ 下载 Release 附件 → minisign 验签 → 静默替换 → 自动重启。
- 发版流程：bump `crates/vqa-tauri/Cargo.toml version`（单一来源）→ build →
  更新 `crates/vqa-tauri/update/latest.json`（version/URL/签名）→ 提交推送 →
  Gitee 建同名 Release 并挂 setup.exe 附件。
- 私钥 `keys/tauri.key` 离线保存（已 gitignore）；丢失则历史版本无法再升级。

### 诊断日志

关于页「诊断日志」：`log_{MAC前8位}_{时间}.txt`（环境元信息 + 页面状态 +
应用日志尾部 200 行 + 崩溃记录）。可导出本地，或上传到仓库 `logs/` 目录
（Gitee Contents API，私人令牌粘贴一次存本地）——维护者 git pull 即读。
