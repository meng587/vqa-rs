# vqa-doctor

视听质量检测工作站的**环境自检与残留清理**小工具。独立 crate，不在 `D:\vqa-rs` 的 workspace 里（自己一个 `target`，不影响主工程构建），零第三方依赖。

## 用法

### 双击（最省事）

直接双击 `target\release\vqa-doctor.exe` 就行：程序会自己判断是不是"双击启动"（新开的独立控制台），是就在末尾停下来等你按回车，结果不会一闪而过；从命令行里跑则立刻返回，不打扰脚本。

- `运行体检.cmd` —— 等价的双击包装，可带参数
- `清理残留.cmd` —— 体检 + 清掉死进程留下的残渣（1 小时以上才动）

### 命令行

```powershell
cd D:\vqa-rs\vqa-doctor
cargo build --release
.\target\release\vqa-doctor.exe            # 体检
.\target\release\vqa-doctor.exe --clean    # 顺手清掉死进程留下的分片与测试空目录
.\target\release\vqa-doctor.exe --json     # 机器可读
.\target\release\vqa-doctor.exe --root D:\vqa-rs
```

退出码：有 ERR 项返回 1，否则 0 —— 方便挂进日常检查脚本。

## 查什么

1. **工具链定位**（与主程序 `tools.rs` 修好后的顺序一致）：环境变量 `VQA_FFMPEG/VQA_FFPROBE/VQA_DAVS2/VQA_MPV` → 共享配置 `dev_root` → 逐级上溯 `<base>\dev` 与 `<base>\音视频工具\dev` → 仓库内便携目录 → `PATH`。命中即打印路径、体积与 `-version` 首行。
2. **数据目录**：`VQA_HOME` 或 `%APPDATA%\vqa`，统计总体积、`shared\bridge_cache`、`scratch`。
3. **仓库内部**：`target` 体积、`bin\mpv\mpv.exe` 是否在、两把 `tauri.key` 是否躺在目录里、三处版本号是否一致、`logs` 是否含主机名、`.mimosa` 与 `dist` 占位。
4. **临时残留**：扫 `数据目录\scratch` 与 `%TEMP%` 下的 `vqa_seg_*` / `vqa_frag_*` / `vqa-test-*`；只有「目录名里的 PID 已不存在，且目录内 1 小时没被改写」才删，正在播的那一份不会被误伤。

默认只读，`--clean` 才会删文件。

## 首次运行结果（2026-11-16）

```
5 OK / 4 WARN / 1 ERR
```

- OK：ffmpeg（`dev\bin`，8.1）、ffprobe（9.0.1）、davs2（`dev\bin\davs2-10bit`）、mpv（`bin\mpv`，v0.42）
- WARN：`keys\tauri.key` 在目录里（已移出版本树）、版本号 0.1.0/0.2.5/0.2.0、`logs` 5 个文件含主机名、临时残留 39 个约 127 MB
- ERR：`crates\vqa-tauri\keys\tauri.key` 是第二把签名私钥

注意：`davs2` 的真实位置是 `D:\音视频工具\dev\bin\davs2-10bit\vkit_davs2.exe`（不是 `dev\davs2-10bit\`）；主程序 `tools.rs` 的候选路径本来就写的是 `bin/davs2-10bit`，所以修复后的定位是通的。
