fn main() {
    // ★ 前端产物是在编译期被 `tauri::generate_context!()` 内嵌进二进制的，
    //   但 cargo 默认不把 ui/dist 当构建输入：只改前端时 `cargo build`
    //   会判定"无事可做"，跑起来的仍是上一版内嵌界面（改了文案却看不到）。
    //   这里显式声明依赖，让 dist 变化触发重编。
    println!("cargo:rerun-if-changed=ui/dist");
    println!("cargo:rerun-if-changed=ui/dist/index.html");
    tauri_build::build()
}
