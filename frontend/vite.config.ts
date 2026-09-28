import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

const backend = process.env.CHAINDEAL_BACKEND ?? 'http://localhost:8080';

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    // Everything server-side lives in the cluster: chain API and the auth service.
    proxy: {
      '/api': { target: backend, changeOrigin: true, secure: true },
      '/oauth': { target: backend, changeOrigin: true, secure: true },
      '/.well-known': { target: backend, changeOrigin: true, secure: true },
    },
  },
  build: { target: 'es2022' },
});
