import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import basicSsl from "@vitejs/plugin-basic-ssl";
import { mockApiPlugin } from "./mock/plugin";

export default defineConfig({
  plugins: [
    react(),
    tailwindcss(),
    mockApiPlugin(),
    process.env.VITE_HTTPS === "1" ? basicSsl() : null,
  ].filter((p): p is Plugin => p !== null),
});
