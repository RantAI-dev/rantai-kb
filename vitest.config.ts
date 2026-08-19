import { defineConfig } from "vitest/config"
import path from "path"

export default defineConfig({
  resolve: {
    alias: { "@": path.resolve(__dirname, "src") },
  },
  test: {
    globals: true,
    include: ["tests/**/*.test.ts"],
    exclude: ["tests/smoke/**"],
    testTimeout: 30000,
    fileParallelism: false,
    // Registers in-memory ports so unit tests never need a database.
    setupFiles: ["./tests/setup-kb-runtime.ts"],
  },
})
