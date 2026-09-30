import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: '127.0.0.1', watch: { ignored: ['**/target/**', '**/src-tauri/**', '**/crates/**'] } },
  build: { target: 'es2022', sourcemap: true },
  test: { environment: 'jsdom', setupFiles: ['./src/test/setup.ts'], include: ['src/**/*.test.ts', 'src/**/*.test.tsx'], restoreMocks: true, maxWorkers: 2, minWorkers: 1 },
});
