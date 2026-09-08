import { fileURLToPath, URL } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri v2 contract (src-tauri/tauri.conf.json):
//   devUrl  = http://localhost:1420   -> server.port 1420, strictPort
//   frontendDist = ../dist            -> build.outDir dist
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    // Explicit IPv4 loopback: on this machine "localhost" resolves to ::1,
    // which falls inside a Windows dynamic port-exclusion range (EACCES).
    // http://localhost:1420 still reaches 127.0.0.1 (tauri devUrl contract).
    host: "127.0.0.1",
    // Tauri dev serves its own HMR websocket; avoid vite's default polling.
    watch: {
      ignored: ["**/src-tauri/**", "**/target/**"],
    },
  },
  build: {
    outDir: "dist",
    target: "chrome105",
    emptyOutDir: true,
  },
  envPrefix: ["VITE_"],
});
