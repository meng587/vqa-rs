//! vqa-doctor —— 视听质量检测工作站的环境自检与残留清理。
//!
//! 独立于 `D:\vqa-rs` 主 workspace（自己一个 crate、自己的 target），只读系统状态，
//! 除了显式 `--clean` 之外不改动任何文件。
//!
//! 用法：
//!   vqa-doctor                 # 体检：工具链 / 数据目录 / 临时残留 / 仓库内部
//!   vqa-doctor --clean         # 顺手清掉死进程留下的分片与测试空目录
//!   vqa-doctor --json          # 机器可读输出
//!   vqa-doctor --root D:\vqa-rs # 指定仓库根（默认当前目录，回退 D:\vqa-rs）

use std::ffi::c_void;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
/// 临时目录里超过这个年纪的残渣才可以删（防止误删正在播的那一份）
const STALE_S: u64 = 3600;

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn CloseHandle(h: *mut c_void) -> i32;
    fn SetConsoleOutputCP(cp: u32) -> i32;
    fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
}

/// 控制台里只有自己 → 是双击/资源管理器启动的（新开的窗口）；
/// 从 cmd/PowerShell 里跑时至少还有一个 shell，计数 ≥ 2。
fn launched_by_double_click() -> bool {
    let mut buf = [0u32; 64];
    let n = unsafe { GetConsoleProcessList(buf.as_mut_ptr(), 64) };
    n <= 1
}

fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            false
        } else {
            CloseHandle(h);
            true
        }
    }
}

/// 输出到 stdout，但忽略"管道已被关闭"（被 `| Select-Object -First N` 提前掐断时不该 panic）
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let mut o = std::io::stdout();
        let _ = writeln!(o, $($arg)*);
    }};
}

// ------------------------------------------------------------------ 报告

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Ok,
    Info,
    Warn,
    Err,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Ok => "OK",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Err => "ERR",
        }
    }
    fn json(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Err => "err",
        }
    }
}

struct Report {
    items: Vec<(Level, String, String)>,
}

impl Report {
    fn new() -> Self {
        Report { items: Vec::new() }
    }
    fn add(&mut self, l: Level, name: impl Into<String>, detail: impl Into<String>) {
        self.items.push((l, name.into(), detail.into()));
    }
    fn section(&self, title: &str) {
        outln!("\n== {title} ==");
    }
    fn dump_section(&self, from: usize, title: &str) {
        self.section(title);
        for (l, n, d) in &self.items[from..] {
            outln!("[{:<4}] {:<12} {}", l.tag(), n, d);
        }
    }
    fn count(&self, l: Level) -> usize {
        self.items.iter().filter(|x| x.0 == l).count()
    }
    fn json(&self) -> String {
        let body: Vec<String> = self
            .items
            .iter()
            .map(|(l, n, d)| {
                format!(
                    "{{\"level\":\"{}\",\"item\":\"{}\",\"detail\":\"{}\"}}",
                    l.json(),
                    esc(n),
                    esc(d)
                )
            })
            .collect();
        format!(
            "{{\"ok\":{},\"warn\":{},\"err\":{},\"items\":[{}]}}",
            self.count(Level::Ok),
            self.count(Level::Warn),
            self.count(Level::Err),
            body.join(",")
        )
    }
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

// ------------------------------------------------------------------ 基础工具

fn human(n: u64) -> String {
    const KB: f64 = 1024.0;
    let n = n as f64;
    if n < KB {
        format!("{n:.0} B")
    } else if n < KB * KB {
        format!("{:.1} KB", n / KB)
    } else if n < KB * KB * KB {
        format!("{:.1} MB", n / (KB * KB))
    } else {
        format!("{:.2} GB", n / (KB * KB * KB))
    }
}

/// 递归统计（不跟随符号链接，不报错中断）
fn dir_stats(p: &Path) -> (usize, u64) {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.filter_map(|e| e.ok()) {
            let md = match e.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if md.is_dir() {
                stack.push(e.path());
            } else if md.is_file() {
                files += 1;
                bytes += md.len();
            }
        }
    }
    (files, bytes)
}

fn home_dir() -> PathBuf {
    if let Ok(v) = std::env::var("VQA_HOME") {
        if !v.trim().is_empty() {
            return PathBuf::from(v);
        }
    }
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    PathBuf::from(appdata).join("vqa")
}

fn ancestors(p: &Path, max: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut cur = Some(p.to_path_buf());
    while let Some(c) = cur {
        out.push(c.clone());
        if out.len() >= max {
            break;
        }
        cur = c.parent().map(|x| x.to_path_buf());
    }
    out
}

/// 从极简 JSON 里取一个字符串字段（够用即可，不引 serde）
fn read_json_string(file: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(file).ok()?;
    let needle = format!("\"{key}\"");
    let at = text.find(&needle)?;
    let rest = &text[at + needle.len()..];
    let colon = rest.find(':')?;
    let rest = &rest[colon + 1..];
    let q1 = rest.find('"')?;
    let rest = &rest[q1 + 1..];
    let q2 = rest.find('"')?;
    let v = rest[..q2].trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// 读 Cargo.toml / package.json 里的 version
fn read_version(file: &Path) -> Option<String> {
    let text = fs::read_to_string(file).ok()?;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("version") && t.contains('=') && !t.starts_with("version.") {
            let v = t.split('=').nth(1)?.trim().trim_matches('"').to_string();
            if !v.is_empty() && !v.contains("workspace") {
                return Some(v);
            }
        }
        if t.starts_with("\"version\"") {
            let v = t.split(':').nth(1)?.trim().trim_matches(',').trim_matches('"');
            return Some(v.to_string());
        }
    }
    None
}

fn first_line_of(exe: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(|s| s.trim().chars().take(72).collect())
}

// ------------------------------------------------------------------ 工具链

struct ToolHit {
    path: Option<PathBuf>,
    how: String,
}

/// 与主程序 `tools.rs` 修好后的定位顺序保持一致：
/// 环境变量 → 共享配置 dev_root → 逐级上溯 dev 树 → 便携目录 → PATH
fn find_tool(var: &str, files: &[&str], root: &Path) -> ToolHit {
    if let Ok(v) = std::env::var(var) {
        if !v.trim().is_empty() {
            let p = PathBuf::from(v.trim());
            if p.is_file() {
                return ToolHit {
                    path: Some(p),
                    how: format!("环境变量 {var}"),
                };
            }
        }
    }
    let cfg = home_dir().join("shared").join("settings.json");
    if let Some(dev) = read_json_string(&cfg, "dev_root") {
        for f in files {
            let p = Path::new(&dev).join(f);
            if p.is_file() {
                return ToolHit {
                    path: Some(p),
                    how: "共享配置 dev_root".into(),
                };
            }
        }
    }
    for base in ancestors(root, 8) {
        for layout in [
            base.join("dev"),
            base.join("音视频工具").join("dev"),
        ] {
            for f in files {
                let p = layout.join(f);
                if p.is_file() {
                    return ToolHit {
                        path: Some(p.clone()),
                        how: format!("上溯 {}", layout.display()),
                    };
                }
            }
        }
    }
    for f in files {
        let p = root.join(f);
        if p.is_file() {
            return ToolHit {
                path: Some(p),
                how: "仓库内便携目录".into(),
            };
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for d in path.split(';').filter(|s| !s.trim().is_empty()) {
            for f in files {
                let p = Path::new(d).join(f);
                if p.is_file() {
                    return ToolHit {
                        path: Some(p),
                        how: "PATH".into(),
                    };
                }
            }
        }
    }
    ToolHit {
        path: None,
        how: "未找到".into(),
    }
}

// ------------------------------------------------------------------ 清理

struct CleanStat {
    scanned: usize,
    removed: usize,
    freed: u64,
    kept: usize,
}

fn newest_mtime(p: &Path) -> Option<SystemTime> {
    let mut newest = fs::metadata(p).ok().and_then(|m| m.modified().ok());
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.filter_map(|e| e.ok()) {
            if let Some(t) = e.metadata().ok().and_then(|m| m.modified().ok()) {
                newest = Some(match newest {
                    Some(n) if n > t => n,
                    _ => t,
                });
            }
        }
    }
    newest
}

/// 目录名里的 PID（`vqa_seg_30236_1759...` / `vqa_frag_30236_xxx`）
fn pid_in_name(name: &str) -> Option<u32> {
    let rest = if let Some(r) = name.strip_prefix("vqa_seg_") {
        r
    } else if let Some(r) = name.strip_prefix("vqa_frag_") {
        r
    } else {
        return None;
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn clean_stale(do_delete: bool) -> CleanStat {
    let mut st = CleanStat {
        scanned: 0,
        removed: 0,
        freed: 0,
        kept: 0,
    };
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(STALE_S))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    for root in [home_dir().join("scratch"), std::env::temp_dir()] {
        let Ok(rd) = fs::read_dir(&root) else { continue };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else { continue };
            let is_seg = name.starts_with("vqa_seg_");
            let is_frag = name.starts_with("vqa_frag_");
            let is_test = name.starts_with("vqa-test-");
            if !(is_seg || is_frag || is_test) {
                continue;
            }
            st.scanned += 1;
            let pid = pid_in_name(name);
            let owner_alive = pid.map(pid_alive).unwrap_or(false);
            let age_ok = newest_mtime(&p).map(|t| t < cutoff).unwrap_or(false);
            // 活着的进程占的目录一律不动；其余要「够老」才删
            if owner_alive || !age_ok {
                st.kept += 1;
                continue;
            }
            if do_delete {
                let (_, bytes) = dir_stats(&p);
                if fs::remove_dir_all(&p).is_ok() {
                    st.removed += 1;
                    st.freed += bytes;
                } else {
                    st.kept += 1;
                }
            } else {
                let (_, bytes) = dir_stats(&p);
                st.removed += 1;
                st.freed += bytes;
            }
        }
    }
    st
}

// ------------------------------------------------------------------ 体检

fn check_toolchain(rep: &mut Report, root: &Path, out: &mut Vec<(Level, String, String)>) {
    let tools: [(&str, &str, &[&str]); 4] = [
        (
            "ffmpeg",
            "VQA_FFMPEG",
            &["bin/ffmpeg.exe", "ffmpeg.exe"],
        ),
        (
            "ffprobe",
            "VQA_FFPROBE",
            &["bin/ffprobe.exe", "ffprobe.exe"],
        ),
        (
            "davs2",
            "VQA_DAVS2",
            &[
                "bin/davs2-10bit/vkit_davs2.exe",
                "davs2-10bit/vkit_davs2.exe",
                "bin/vkit_davs2.exe",
                "vkit_davs2.exe",
            ],
        ),
        (
            "mpv",
            "VQA_MPV",
            &["bin/mpv/mpv.exe", "bin/mpv.exe", "mpv.exe"],
        ),
    ];
    for (label, var, files) in tools {
        let hit = find_tool(var, files, root);
        match &hit.path {
            Some(p) => {
                let mut detail = format!("{}（{}，{}）", p.display(), hit.how, human(fs::metadata(p).map(|m| m.len()).unwrap_or(0)));
                if let Some(v) = first_line_of(p, &["-version"]) {
                    detail.push_str(&format!(" · {v}"));
                }
                rep.add(Level::Ok, label, detail.clone());
                out.push((Level::Ok, label.into(), detail));
            }
            None => {
                let detail = format!(
                    "未找到：VQA_{} 未设、共享配置无 dev_root、上溯未命中、PATH 无。{}",
                    var.trim_start_matches("VQA_"),
                    if label == "davs2" {
                        "（AVS2 并行桥会退化，只能软件解码）"
                    } else {
                        ""
                    }
                );
                rep.add(Level::Err, label, detail.clone());
                out.push((Level::Err, label.into(), detail));
            }
        }
    }
}

fn check_data_dir(rep: &mut Report, out: &mut Vec<(Level, String, String)>) {
    let home = home_dir();
    if !home.is_dir() {
        rep.add(Level::Warn, "数据目录", format!("{} 不存在（首次运行会自动建）", home.display()));
        out.push((Level::Warn, "数据目录".into(), home.display().to_string()));
        return;
    }
    let (files, bytes) = dir_stats(&home);
    rep.add(
        Level::Info,
        "数据目录",
        format!("{}（{files} 文件，{}）", home.display(), human(bytes)),
    );
    out.push((Level::Info, "数据目录".into(), home.display().to_string()));

    let cache = home.join("shared").join("bridge_cache");
    if cache.is_dir() {
        let (n, b) = dir_stats(&cache);
        let l = if b > 2u64 * 1024 * 1024 * 1024 { Level::Warn } else { Level::Info };
        rep.add(l, "桥缓存", format!("{n} 文件，{}（上限应为 ≤2 GB）", human(b)));
        out.push((l, "桥缓存".into(), human(b)));
    }
    let scratch = home.join("scratch");
    if scratch.is_dir() {
        let (n, b) = dir_stats(&scratch);
        rep.add(Level::Info, "scratch", format!("{n} 文件，{}", human(b)));
        out.push((Level::Info, "scratch".into(), human(b)));
    } else {
        rep.add(
            Level::Info,
            "scratch",
            "尚未创建（首次转码时由程序建立；分片与片段将落这里，不再吃 C 盘临时目录）",
        );
        out.push((Level::Info, "scratch".into(), "不存在".into()));
    }
}

fn check_repo(rep: &mut Report, root: &Path) {
    if !root.is_dir() {
        rep.add(Level::Err, "仓库根", format!("{} 不存在", root.display()));
        return;
    }
    let target = root.join("target");
    if target.is_dir() {
        let (n, b) = dir_stats(&target);
        rep.add(
            Level::Info,
            "target",
            format!("{n} 文件，{}（编译缓存，`cargo clean` 可回收）", human(b)),
        );
    }
    let mpv = root.join("bin").join("mpv").join("mpv.exe");
    if mpv.is_file() {
        rep.add(
            Level::Ok,
            "bin/mpv",
            format!(
                "{}（打便携包必需，未入库）",
                human(fs::metadata(&mpv).map(|m| m.len()).unwrap_or(0))
            ),
        );
    } else {
        rep.add(Level::Warn, "bin/mpv", "mpv.exe 缺失，便携包会打不出来");
    }
    // 两把私钥都在目录里
    let k1 = root.join("keys").join("tauri.key");
    let k2 = root.join("crates").join("vqa-tauri").join("keys").join("tauri.key");
    if k1.is_file() {
        rep.add(
            Level::Warn,
            "私钥",
            format!("{} 存在（已移出版本树，但文件在目录里）", k1.display()),
        );
    }
    if k2.is_file() {
        rep.add(
            Level::Err,
            "私钥",
            format!("{} 是第二把签名私钥（key id 与根目录那把不同），拷贝即泄露", k2.display()),
        );
    }
    if !k1.is_file() && !k2.is_file() {
        rep.add(Level::Ok, "私钥", "目录内无 tauri.key");
    }
    // 版本三处
    let vs = [
        ("workspace", root.join("Cargo.toml")),
        (
            "vqa-tauri",
            root.join("crates").join("vqa-tauri").join("Cargo.toml"),
        ),
        (
            "ui",
            root.join("crates").join("vqa-tauri").join("ui").join("package.json"),
        ),
    ];
    let mut versions = Vec::new();
    for (name, f) in vs {
        versions.push((name, read_version(&f)));
    }
    let uniq: Vec<&str> = {
        let mut v: Vec<&str> = versions.iter().filter_map(|(_, x)| x.as_deref()).collect();
        v.sort();
        v.dedup();
        v
    };
    let text: Vec<String> = versions
        .iter()
        .map(|(n, v)| format!("{n}={}", v.as_deref().unwrap_or("?")))
        .collect();
    if uniq.len() <= 1 {
        rep.add(Level::Ok, "版本号", text.join("，"));
    } else {
        rep.add(Level::Warn, "版本号", format!("{}（三处不一致）", text.join("，")));
    }
    // 诊断日志里有没有设备指纹
    let logs = root.join("logs");
    if logs.is_dir() {
        let mut hits = 0;
        if let Ok(rd) = fs::read_dir(&logs) {
            for e in rd.filter_map(|e| e.ok()) {
                if !e.path().is_file() {
                    continue;
                }
                let text = fs::read_to_string(e.path()).unwrap_or_default();
                if text.contains("LAPTOP-") || text.contains("主机名") {
                    hits += 1;
                }
            }
        }
        if hits > 0 {
            rep.add(Level::Warn, "logs", format!("{hits} 个诊断文件含主机名/设备信息，且已入库"));
        } else {
            rep.add(Level::Ok, "logs", "未见设备指纹");
        }
    }
    let mimosa = root.join(".mimosa");
    if mimosa.is_dir() {
        let (n, b) = dir_stats(&mimosa);
        rep.add(Level::Info, ".mimosa", format!("{n} 文件，{}（助手状态，已忽略）", human(b)));
    }
    let dist = root.join("dist");
    if dist.is_dir() {
        let (n, b) = dir_stats(&dist);
        rep.add(Level::Info, "dist", format!("{n} 文件，{}（旧便携包，可清）", human(b)));
    }
}

fn print_help() {
    outln!("vqa-doctor —— 视听质量检测工作站环境自检");
    outln!();
    outln!("  vqa-doctor                体检");
    outln!("  vqa-doctor --clean        清理死进程留下的分片/测试空目录（1 小时以上才动）");
    outln!("  vqa-doctor --json         机器可读");
    outln!("  vqa-doctor --root <path>  指定仓库根");
}

fn default_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    if cwd.join("crates").join("vqa-tauri").is_dir() {
        return cwd;
    }
    // 从 exe 位置往上找仓库根（doctor 自己也在仓库里）
    let mut base: Option<PathBuf> = std::env::current_exe().ok();
    while let Some(p) = base {
        if p.join("crates").join("vqa-tauri").is_dir() {
            return p;
        }
        base = p.parent().map(|x| x.to_path_buf());
    }
    PathBuf::from(r"D:\vqa-rs")
}

fn main() {
    unsafe {
        SetConsoleOutputCP(65001);
    }
    let mut json = false;
    let mut clean = false;
    let mut root: Option<PathBuf> = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--clean" => clean = true,
            "--root" => {
                i += 1;
                match args.get(i) {
                    Some(v) => root = Some(PathBuf::from(v)),
                    None => {
                        eprintln!("--root 需要一个路径");
                        std::process::exit(2);
                    }
                }
            }
            "-h" | "--help" => {
                print_help();
                return;
            }
            other => {
                eprintln!("未知参数：{other}");
                print_help();
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let root = root.unwrap_or_else(default_root);
    let mut rep = Report::new();
    let mut json_items: Vec<(Level, String, String)> = Vec::new();

    // 1. 工具链
    let mark = rep.items.len();
    check_toolchain(&mut rep, &root, &mut json_items);
    if !json {
        rep.dump_section(mark, "1. 工具链定位");
    }

    // 2. 数据目录
    let mark = rep.items.len();
    check_data_dir(&mut rep, &mut json_items);
    if !json {
        rep.dump_section(mark, "2. 数据目录");
    }

    // 3. 仓库
    let mark = rep.items.len();
    check_repo(&mut rep, &root);
    if !json {
        rep.dump_section(mark, "3. 仓库内部");
    }

    // 4. 临时残留
    let mark = rep.items.len();
    let st = clean_stale(clean);
    let detail = if clean {
        format!(
            "扫描 {} 个，删除 {} 个，回收 {}，保留 {} 个（进程存活或未满 1 小时）",
            st.scanned,
            st.removed,
            human(st.freed),
            st.kept
        )
    } else {
        format!(
            "扫描 {} 个，可删 {} 个（约 {}），保留 {} 个；加 --clean 执行",
            st.scanned,
            st.removed,
            human(st.freed),
            st.kept
        )
    };
    let lvl = if st.scanned == 0 {
        Level::Ok
    } else if clean {
        Level::Info
    } else {
        Level::Warn
    };
    rep.add(lvl, "临时残留", detail);
    if !json {
        rep.dump_section(mark, "4. 临时残留");
    }

    // 汇总
    if json {
        outln!("{}", rep.json());
    } else {
        outln!(
            "\n汇总：{} OK / {} WARN / {} ERR（仓库根 {}）",
            rep.count(Level::Ok),
            rep.count(Level::Warn),
            rep.count(Level::Err),
            root.display()
        );
        if rep.count(Level::Err) > 0 {
            outln!("有 ERR 项，先按上面的路径处理；加 --clean 可清临时残留。");
        }
    }
    let _ = json_items; // 保留给后续结构化输出扩展

    // 双击启动时窗口会随进程退出立刻消失，这里等一下，让结果能看完
    if !json && launched_by_double_click() {
        outln!("\n按回车关闭窗口…");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
    std::process::exit(if rep.count(Level::Err) > 0 { 1 } else { 0 });
}
