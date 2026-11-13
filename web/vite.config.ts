import { defineConfig } from "vite";

// 開発時は API を Rust サーバー（既定 127.0.0.1:8787）へ中継する。
export default defineConfig({
  server: {
    proxy: { "/api": "http://127.0.0.1:8787" },
  },
});
