import { z } from "zod";

/** A document as served by the daemon's /api/documents endpoints. */
export const DocumentSchema = z.object({
  id: z.string(),
  doc_type: z.string(),
  surface: z.string().nullable(),
  status: z.string(),
  priority: z.string().nullable(),
  owner: z.string(),
  payload: z.string(),
  archived_at: z.string().nullable().optional(),
  created_at: z.string().optional(),
  updated_at: z.string().optional(),
});

export type Document = z.infer<typeof DocumentSchema>;

export const DocumentListSchema = z.array(DocumentSchema);

/** Parse the payload column once into `unknown`; callers narrow with guards. */
export function parsePayload(raw: string): unknown {
  try {
    return JSON.parse(raw) as unknown;
  } catch {
    return null;
  }
}
