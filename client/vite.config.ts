/// <reference types="vitest" />
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import path from 'path'
import fs from 'fs'

const host = process.env.TAURI_DEV_HOST
const defaultServerUrl = process.env.SAYIT_DEFAULT_SERVER_URL || 'https://sayitapp.site'

// 從 tauri.conf.json 讀取版本號
const tauriConf = JSON.parse(
  fs.readFileSync(path.resolve(__dirname, 'src-tauri/tauri.conf.json'), 'utf-8')
)
// SAYIT_FAKE_APP_VERSION：只為「在真機上測自動更新」存在。
// 把前端自報的版本壓低（如 0.1.5），線上 manifest 的 0.1.6 就成了"新版本"，
// 整條鏈路（檢查 → 下載 → SHA-512 校驗 → 提醒圖示 → 安裝）都能真跑一遍，
// 不用先發一個假包上去。發版前必須測自更新（見 .kiro/steering/pitfalls.md #3），
// 而更新器的改動沒法用自己驗證自己，這個口子是那道驗證的入口。
// 正式構建不會設這個變數；設了會打一行醒目的警告。
const fakeVersion = process.env.SAYIT_FAKE_APP_VERSION
if (fakeVersion) {
  console.warn(`\n[vite] SAYIT_FAKE_APP_VERSION=${fakeVersion} — 應用會自報這個版本（真實版本 ${tauriConf.version}）。僅用於測更新，別拿來打包。\n`)
}
const appVersion = fakeVersion || tauriConf.version || '0.0.0'

export default defineConfig({
  define: {
    __SAYIT_DEFAULT_SERVER_URL__: JSON.stringify(defaultServerUrl),
    __APP_VERSION__: JSON.stringify(appVersion),
  },
  plugins: [react()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  // Vite options tailored for Tauri development
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: 'ws', host, port: 1421 } : undefined,
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
  test: {
    globals: true,
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
  build: {
    // 生產構建移除 console.log 和 debugger（保留 console.warn/error）
    minify: 'esbuild',
    rollupOptions: {
      input: {
        main: path.resolve(__dirname, 'index.html'),
        overlay: path.resolve(__dirname, 'overlay.html'),
        updateNotification: path.resolve(__dirname, 'update-notification.html'),
      },
    },
  },
  esbuild: {
    drop: ['debugger'],
    pure: ['console.log'],
  },
})
