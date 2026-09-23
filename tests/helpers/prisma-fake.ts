import { vi } from "vitest"

/**
 * A minimal in-memory stand-in for the slice of Prisma's query API the new
 * `/v1/documents`, `/v1/jobs/:id/retry`, `/v1/knowledge-bases/:id` and
 * `/v1/categories` routes use.
 *
 * Real Prisma filters rows on the WHERE clause the route builds — most
 * importantly `tenantId: auth.tenantId`, which is what makes "another
 * tenant's row" and "no such row" produce the identical `null`. A fake that
 * ignores `where` and just returns canned values (or always returns the same
 * row) can't tell a route with real tenant scoping apart from one that lost
 * it, so this fake actually evaluates `where` against the stored rows —
 * exactly the property the tenancy tests need to be able to fail.
 */

type Where = Record<string, unknown>

function matches(row: Record<string, unknown>, where: Where): boolean {
  return Object.entries(where).every(([key, cond]) => {
    const value = row[key]
    if (cond === null) return value === null || value === undefined
    if (cond && typeof cond === "object" && !Array.isArray(cond)) {
      const c = cond as { in?: unknown[]; not?: unknown; has?: unknown; some?: Where }
      if ("in" in c) return Array.isArray(c.in) && c.in.includes(value)
      if ("not" in c) return c.not === null ? value !== null && value !== undefined : value !== c.not
      if ("has" in c) return Array.isArray(value) && value.includes(c.has)
      if ("some" in c && c.some) return Array.isArray(value) && value.some((v) => matches(v, c.some as Where))
    }
    return value === cond
  })
}

/** Prisma `select: {...}` projects the returned object down to just the
 *  requested fields — tested code (categories, in particular) relies on that
 *  to keep tenantId out of an HTTP response. `true` copies the field as-is; a
 *  nested object (e.g. `groups: { select: { knowledgeBaseId: true } }`) also
 *  just copies the field as-is, since every fake row already stores relation
 *  fields pre-shaped the way the real Prisma include/select would return them
 *  (see the `groups` getters set up in the route tests). */
function project<T extends Record<string, unknown>>(row: T, select?: Record<string, unknown>): T {
  if (!select) return row
  const out: Record<string, unknown> = {}
  for (const key of Object.keys(select)) {
    if (select[key]) out[key] = row[key]
  }
  return out as T
}

/** Apply Prisma's `{ increment: n }` / `{ decrement: n }` field-update
 *  operators, which the retry route uses on `attempt`. Every other field is a
 *  plain assignment. */
function applyUpdate<T extends Record<string, unknown>>(row: T, data: Record<string, unknown>): void {
  for (const [key, value] of Object.entries(data)) {
    if (value && typeof value === "object" && !Array.isArray(value) && !(value instanceof Date)) {
      const op = value as { increment?: number; decrement?: number }
      if (typeof op.increment === "number") {
        ;(row as Record<string, unknown>)[key] = (Number(row[key]) || 0) + op.increment
        continue
      }
      if (typeof op.decrement === "number") {
        ;(row as Record<string, unknown>)[key] = (Number(row[key]) || 0) - op.decrement
        continue
      }
    }
    ;(row as Record<string, unknown>)[key] = value
  }
}

export class FakeTable<T extends { id: string }> {
  rows: T[]
  constructor(rows: T[] = []) {
    this.rows = rows
  }

  findFirst = vi.fn(async ({ where, select }: { where: Where; select?: Record<string, unknown> }) => {
    const row = this.rows.find((r) => matches(r, where))
    return row ? project(row, select) : null
  })

  findUnique = vi.fn(async ({ where, select }: { where: Where; select?: Record<string, unknown> }) => {
    const row = this.rows.find((r) => matches(r, where))
    return row ? project(row, select) : null
  })

  findMany = vi.fn(async ({ where = {}, select }: { where?: Where; select?: Record<string, unknown> } = {}) =>
    this.rows.filter((r) => matches(r, where)).map((r) => project(r, select))
  )

  create = vi.fn(async ({ data }: { data: Partial<T> & { id?: string } }) => {
    const row = { id: data.id ?? `id_${this.rows.length + 1}`, ...data } as T
    this.rows.push(row)
    return row
  })

  createMany = vi.fn(async ({ data }: { data: Array<Partial<T> & { id?: string }> }) => {
    for (const d of data) {
      const row = { id: d.id ?? `id_${this.rows.length + 1}`, ...d } as T
      this.rows.push(row)
    }
    return { count: data.length }
  })

  update = vi.fn(async ({ where, data }: { where: Where; data: Record<string, unknown> }) => {
    const row = this.rows.find((r) => matches(r, where))
    if (!row) throw new Error("[prisma-fake] update: no row matched")
    applyUpdate(row as Record<string, unknown>, data)
    return row
  })

  updateMany = vi.fn(async ({ where, data }: { where: Where; data: Record<string, unknown> }) => {
    const matched = this.rows.filter((r) => matches(r, where))
    for (const row of matched) applyUpdate(row as Record<string, unknown>, data)
    return { count: matched.length }
  })

  delete = vi.fn(async ({ where }: { where: Where }) => {
    const idx = this.rows.findIndex((r) => matches(r, where))
    if (idx === -1) throw new Error("[prisma-fake] delete: no row matched")
    const [row] = this.rows.splice(idx, 1)
    return row
  })

  deleteMany = vi.fn(async ({ where = {} }: { where?: Where } = {}) => {
    const before = this.rows.length
    this.rows = this.rows.filter((r) => !matches(r, where))
    return { count: before - this.rows.length }
  })
}

/** `$transaction` just runs each promise/thunk in order — no real atomicity
 *  needed for these tests, only that every statement executes. */
export async function fakeTransaction(ops: Array<Promise<unknown>>): Promise<unknown[]> {
  return Promise.all(ops)
}
