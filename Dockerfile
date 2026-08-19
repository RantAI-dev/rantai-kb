# syntax=docker/dockerfile:1
FROM oven/bun:1.1-debian AS deps
WORKDIR /app
COPY package.json bun.lock* ./
RUN bun install --frozen-lockfile || bun install

FROM oven/bun:1.1-debian AS build
WORKDIR /app
COPY --from=deps /app/node_modules ./node_modules
COPY . .
# Generate the Prisma client for the runtime platform.
RUN bunx prisma generate

FROM oven/bun:1.1-debian AS runtime
WORKDIR /app
ENV NODE_ENV=production
# Ingest shells out to nothing, but PDFs and OCR benefit from fonts being present.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates fonts-liberation \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /app /app
EXPOSE 8080
# Migrations + vector schema are idempotent, so running them on boot keeps a
# single-command deploy honest. Set KB_SKIP_MIGRATE=1 to manage them yourself.
CMD ["sh", "-c", "if [ \"$KB_SKIP_MIGRATE\" != \"1\" ]; then bunx prisma migrate deploy && bun scripts/apply-vector-schema.ts; fi; exec bun src/server.ts"]
