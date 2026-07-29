import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      "/health": "http://127.0.0.1:8000",
      "/sync": {
        target: "ws://127.0.0.1:8000",
        ws: true,
      },
    },
    allowedHosts: ["horus-linux"],
  },
});
