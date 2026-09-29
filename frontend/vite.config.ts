import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

const backend = process.env.CHAINDEAL_BACKEND ?? 'http://localhost:8080';
// Identity can be routed separately, e.g. a local chain node with the cluster's auth.
const auth = process.env.CHAINDEAL_AUTH ?? backend;
// gRPC-Web (LedgerService): the ingress in the cluster, or a local node's gRPC port.
const grpc = process.env.CHAINDEAL_GRPC ?? backend;

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    // Everything server-side lives in the cluster: chain API and the auth service.
    proxy: {
      '/api': { target: backend, changeOrigin: true, secure: true },
      '/oauth': { target: auth, changeOrigin: true, secure: true },
      '/.well-known': { target: auth, changeOrigin: true, secure: true },
      '/chaindeal.v1.LedgerService': { target: grpc, changeOrigin: true, secure: true },
    },
  },
  build: { target: 'es2022' },
});
