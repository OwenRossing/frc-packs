import { defineConfig } from "vite";

// In development the Rust server runs on :3000 and Vite proxies the API, photos and pack recipes to it.
// In production the server serves the built site (web/dist) itself, so everything is one origin.
const server = process.env.API_URL ?? "http://127.0.0.1:3000";

export default defineConfig({
  server: {
    proxy: { "/api": server, "/photos": server, "/packs": server },
  },
});
