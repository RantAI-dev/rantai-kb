import { createHash, randomBytes, timingSafeEqual } from "node:crypto"
import { prisma } from "./db"

/**
 * API-key authentication.
 *
 * Keys are `rkb_<32 random bytes, base64url>`; only the SHA-256 is stored, so a
 * database leak does not hand out working credentials. Every request resolves
 * to a tenant — there is no unauthenticated path to document data.
 */

export interface AuthContext {
  tenantId: string
  scopes: string[]
  /** Empty = the whole tenant. */
  knowledgeBaseIds: string[]
  keyId: string
}

export function generateApiKey(): { plaintext: string; hash: string } {
  const plaintext = `rkb_${randomBytes(32).toString("base64url")}`
  return { plaintext, hash: hashKey(plaintext) }
}

export function hashKey(plaintext: string): string {
  return createHash("sha256").update(plaintext).digest("hex")
}

/** Constant-time compare so a wrong key can't be discovered by timing. */
function sameHash(a: string, b: string): boolean {
  const bufA = Buffer.from(a, "hex")
  const bufB = Buffer.from(b, "hex")
  return bufA.length === bufB.length && timingSafeEqual(bufA, bufB)
}

export async function authenticate(request: Request): Promise<AuthContext | null> {
  const header = request.headers.get("authorization")
  const bearer = header?.toLowerCase().startsWith("bearer ") ? header.slice(7).trim() : null
  const raw = bearer || request.headers.get("x-api-key")
  if (!raw) return null

  const hash = hashKey(raw)
  const key = await prisma.apiKey.findUnique({ where: { hash } })
  if (!key || key.revokedAt || !sameHash(key.hash, hash)) return null

  // Best-effort usage stamp; never blocks the request.
  void prisma.apiKey.update({ where: { id: key.id }, data: { lastUsedAt: new Date() } }).catch(() => {})

  return {
    tenantId: key.tenantId,
    scopes: key.scopes,
    knowledgeBaseIds: key.knowledgeBaseIds,
    keyId: key.id,
  }
}

export function hasScope(auth: AuthContext, scope: string): boolean {
  return auth.scopes.length === 0 || auth.scopes.includes(scope)
}

/**
 * Narrow a caller-supplied KB filter to what the key is allowed to see.
 * A key bound to specific knowledge bases can never widen its own scope.
 */
export function restrictKnowledgeBases(auth: AuthContext, requested?: string[]): string[] | undefined {
  if (auth.knowledgeBaseIds.length === 0) return requested
  if (!requested || requested.length === 0) return auth.knowledgeBaseIds
  return requested.filter((id) => auth.knowledgeBaseIds.includes(id))
}
