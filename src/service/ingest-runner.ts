import { prisma } from "./db"
import { withTenant } from "./adapters"
import { kb } from "@/lib/kb-runtime/runtime"
import type { JobRecord } from "@/lib/kb-runtime/ports"
import type { StepProgress } from "@/lib/ingest/progress"
import { resolveIngestPolicy, parseFigureMode } from "@/lib/ingest/pipeline-policy"
import { extractDocumentText } from "@/lib/ingest/extract"
import { indexDocumentContent } from "@/lib/ingest/index-document"
import { recordIngestJobFailure, recordIngestJobSuccess } from "@/lib/ingest/job"

/**
 * Worker entry point: run a claimed job to completion.
 *
 * Mirrors what the monolith did inside its knowledge service, minus everything
 * that was app-specific (permissions, quota, audit): download the stored file,
 * extract, index, flip the document to ready. Extraction failures are terminal
 * — a document whose text could not be read must never be embedded, or RAG
 * surfaces placeholder strings as answers.
 */
export async function processIngestJob(
  job: JobRecord,
  onProgress?: (sp: StepProgress) => void | Promise<void>
): Promise<"ready" | "failed"> {
  const tenantId = job.organizationId
  if (!tenantId) {
    await recordIngestJobFailure(job.id, "job has no tenant")
    return "failed"
  }

  return withTenant(tenantId, async () => {
    const markFailed = async (reason: string) => {
      await recordIngestJobFailure(job.id, reason)
      if (job.documentId) {
        await prisma.document.update({ where: { id: job.documentId }, data: { status: "failed" } }).catch(() => {})
      }
    }

    if (!job.documentId || !job.s3Key) {
      await markFailed("job missing documentId or s3Key")
      return "failed"
    }

    const params = (job.params ?? {}) as {
      figureMode?: string
      forceOCR?: boolean
      documentType?: string
      title?: string
      categories?: string[]
      subcategory?: string | null
      useCombined?: boolean
    }

    let fileBuffer: Buffer
    try {
      fileBuffer = await kb("blob").download(job.s3Key)
    } catch (err) {
      await markFailed(`blob download failed: ${(err as Error).message ?? "unknown"}`)
      return "failed"
    }

    const policy = resolveIngestPolicy(job.filename, parseFigureMode(params.figureMode, params.forceOCR))
    const emit = onProgress
      ? (step: StepProgress["step"], current?: number, total?: number) => onProgress({ step, current, total })
      : undefined

    await emit?.("extracting")
    const extraction = await extractDocumentText(
      { name: job.filename, type: job.mimeType || "application/octet-stream" },
      fileBuffer,
      policy,
      { documentType: params.documentType }
    )
    if (extraction.error) {
      await markFailed(extraction.error)
      return "failed"
    }

    const title = params.title || job.filename.replace(/\.[^/.]+$/, "")
    const sanitize = (s: string) => s.replace(/\0/g, "")

    await prisma.document.update({
      where: { id: job.documentId },
      data: {
        content: sanitize(extraction.content),
        fileType: extraction.fileType,
        fileSize: fileBuffer.length,
      },
    })

    try {
      const indexed = await indexDocumentContent(
        {
          documentId: job.documentId,
          title,
          content: sanitize(extraction.content),
          categories: params.categories ?? [],
          subcategory: params.subcategory ?? null,
          organizationId: tenantId,
          userId: job.userId,
          policy,
          useCombined: params.useCombined !== false,
          figures: extraction.figures,
          pagesBlocks: extraction.pagesBlocks,
          pageMap: extraction.pageMap,
        },
        emit
      )
      await prisma.document.update({ where: { id: job.documentId }, data: { status: "ready" } })
      await emit?.("done")
      await recordIngestJobSuccess(job.id, job.documentId)
      console.log(`[kb] ingested ${job.filename} → ${indexed.chunks.length} chunks`)
      return "ready"
    } catch (err) {
      // The stored file is kept so the job can be retried without re-upload;
      // the reaper deletes it once the retry window closes.
      console.error(`[kb] indexing failed for ${job.documentId}:`, err)
      await markFailed((err as Error).message ?? "indexing failed")
      return "failed"
    }
  })
}
