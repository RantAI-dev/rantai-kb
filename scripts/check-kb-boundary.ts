#!/usr/bin/env bun
/**
 * Fails if the KB engine imports app infrastructure.
 *
 * The engine depends only on its ports (lib/kb-runtime/ports); the bindings to
 * Postgres, SurrealDB and object storage live in src/service/adapters.ts. That
 * separation is what lets the same engine run embedded in a host application
 * and standalone in this service — keeping it enforced keeps both true.
 */
import { readdirSync, readFileSync, statSync } from "fs"
import path from "path"

const ENGINE_DIRS = [
  "src/lib/rag",
  "src/lib/ingest",
  "src/lib/ocr",
  "src/lib/document-intelligence",
  "src/lib/files",
]

const DENY = [
  "@/lib/prisma",
  "@prisma/client",
  "@/lib/s3",
  "@/lib/socket",
  "@/lib/surrealdb",
  "@/service/",
  "@/server",
]

const DENY_EXACT: string[] = []

function walk(dir: string): string[] {
  const out: string[] = []
  let entries: string[]
  try {
    entries = readdirSync(dir)
  } catch {
    return out
  }
  for (const entry of entries) {
    const full = path.join(dir, entry)
    if (statSync(full).isDirectory()) out.push(...walk(full))
    else if (/\.tsx?$/.test(entry)) out.push(full)
  }
  return out
}

const violations: string[] = []
for (const dir of ENGINE_DIRS) {
  for (const file of walk(dir)) {
    const lines = readFileSync(file, "utf-8").split("\n")
    lines.forEach((line, i) => {
      // Matches both `from "x"` and `import("x")` (static + lazy).
      const m = line.match(/(?:from\s+|import\(\s*)["']([^"']+)["']/)
      if (!m) return
      const spec = m[1]
      if (DENY.some((d) => spec === d || spec.startsWith(d)) || DENY_EXACT.includes(spec)) {
        violations.push(`${file}:${i + 1} → ${spec}`)
        return
      }

      // Relative imports that climb out of the engine are the same violation
      // wearing a disguise (`../surrealdb` hurts exactly as much as
      // `@/lib/surrealdb` once the engine lives in another repo).
      if (spec.startsWith(".")) {
        const resolved = path.normalize(path.join(path.dirname(file), spec))
        const insideEngine = ENGINE_DIRS.some(
          (d) => resolved === d || resolved.startsWith(d + path.sep)
        )
        const allowedOutside =
          resolved.startsWith(path.join("src", "lib", "kb-runtime", "ports")) ||
          resolved.startsWith(path.join("src", "lib", "kb-runtime", "runtime"))
        if (!insideEngine && !allowedOutside) {
          violations.push(`${file}:${i + 1} → ${spec}  (escapes the engine)`)
        }
      }
    })
  }
}

if (violations.length > 0) {
  console.error(`\n✗ KB boundary: ${violations.length} forbidden import(s)\n`)
  for (const v of violations) console.error("  " + v)
  console.error("\nThe engine must go through lib/kb-runtime/ports instead.\n")
  process.exit(1)
}

console.log("✓ KB boundary clean")
