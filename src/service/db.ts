import { PrismaClient } from "@prisma/client"

// Held on globalThis so a hot reload doesn't open a second pool.
const globalForPrisma = globalThis as unknown as { prisma?: PrismaClient }

export const prisma =
  globalForPrisma.prisma ?? new PrismaClient({ datasourceUrl: process.env.DATABASE_URL })

if (process.env.NODE_ENV !== "production") globalForPrisma.prisma = prisma
