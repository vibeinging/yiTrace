import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// 构建成相对路径的静态资源 → 方便用 rust-embed 塞进引擎单二进制、气隙部署。
export default defineConfig({
  plugins: [react()],
  base: './',
  build: { outDir: 'dist', assetsDir: 'assets', target: 'es2020' },
  server: { port: 5180 },
})
