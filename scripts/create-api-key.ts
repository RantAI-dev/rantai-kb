#!/usr/bin/env bun
/**
 * Mint an API key.
 *
 *   bun scripts/create-api-key.ts <tenantId> [name] [--scopes kb:read,kb:write] [--kb <id,id>]
 *
 * The plaintext is printed once and never stored — only its SHA-256 goes to
 * the database, so losing it means minting a new one.
 */
import { prisma } from "../src/service/db"
import { generateApiKey } from "../src/service/auth"

const args = process.argv.slice(2)
const positional = args.filter((a) => !a.startsWith("--"))
const tenantId = positional[0]
const name = positional[1] ?? "default"

if (!tenantId) {
  console.error("usage: bun scripts/create-api-key.ts <tenantId> [name] [--scopes a,b] [--kb id,id]")
  process.exit(1)
}

function flag(flagName: string): string[] {
  const idx = args.indexOf(flagName)
  if (idx === -1 || !args[idx + 1]) return []
  return args[idx + 1].split(",").map((s) => s.trim()).filter(Boolean)
}

const { plaintext, hash } = generateApiKey()

const key = await prisma.apiKey.create({
  data: {
    tenantId,
    name,
    hash,
    scopes: flag("--scopes"),
    knowledgeBaseIds: flag("--kb"),
  },
})

console.log("")
console.log("  API key created — copy it now, it will not be shown again:")
console.log("")
console.log(`    ${plaintext}`)
console.log("")
console.log(`  id:      ${key.id}`)
console.log(`  tenant:  ${key.tenantId}`)
console.log(`  scopes:  ${key.scopes.length ? key.scopes.join(", ") : "(all)"}`)
console.log(`  KBs:     ${key.knowledgeBaseIds.length ? key.knowledgeBaseIds.join(", ") : "(all in tenant)"}`)
console.log("")

process.exit(0)
