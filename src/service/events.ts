/**
 * Ingest progress fan-out.
 *
 * The monolith pushed progress over Socket.io into an org room. A standalone
 * service has no socket server and no session, so progress is exposed as
 * server-sent events on `GET /v1/events?tenant=…`: any number of subscribers,
 * no handshake, works through every proxy. Clients that prefer polling can use
 * `GET /v1/jobs/:id` instead — the job row carries the same numbers.
 */

type Subscriber = (payload: string) => void

const subscribers = new Map<string, Set<Subscriber>>()

export function subscribe(tenantId: string, fn: Subscriber): () => void {
  let set = subscribers.get(tenantId)
  if (!set) {
    set = new Set()
    subscribers.set(tenantId, set)
  }
  set.add(fn)
  return () => {
    set!.delete(fn)
    if (set!.size === 0) subscribers.delete(tenantId)
  }
}

export function emitProgress(tenantId: string, event: string, payload: Record<string, unknown>): void {
  const set = subscribers.get(tenantId)
  if (!set || set.size === 0) return
  const frame = `event: ${event}\ndata: ${JSON.stringify(payload)}\n\n`
  for (const fn of set) {
    try {
      fn(frame)
    } catch {
      /* a dead subscriber must not break the others */
    }
  }
}

export function subscriberCount(): number {
  let n = 0
  for (const set of subscribers.values()) n += set.size
  return n
}
