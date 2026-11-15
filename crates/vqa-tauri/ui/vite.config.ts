import { defineConfig } from "vite";

export default defineConfig({
  // 显式绑 IPv4——vite 默认 localhost 可能只听 ::1，WebView2 解析到 127.0.0.1 会拒连
  server: { port: 5173, strictPort: true, host: "127.0.0.1" },
  build: { target: "chrome110" },
});
