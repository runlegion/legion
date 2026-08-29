import type { ZodType } from "zod";

/** A failed request, kept until the region showing it is replaced or cleared. */
export interface FetchFailure {
  request: string;
  message: string;
}

export function describeError(err: unknown): string {
  if (err instanceof Error) return err.message;
  return "unknown error";
}

export function documentPath(id: string, suffix = ""): string {
  return `/api/documents/${encodeURIComponent(id)}${suffix}`;
}

interface RequestOptions {
  method?: string;
  body?: unknown;
  signal?: AbortSignal;
}

/**
 * The reason a request failed, in the daemon's own words where it gives them.
 * Refusals arrive in three shapes -- a JSON envelope ({"error": "..."} for a
 * rejected payload or an archived document), plain text (a 422 from the JSON
 * extractor), and no body at all (a 413 over the size cap) -- so the body is
 * read as text and only then tried as JSON. A bare status is the last resort:
 * "is archived and cannot be edited" is the half worth reading.
 */
async function refusal(res: Response): Promise<Error> {
  const status = `request failed with status ${res.status}`;
  let text: string;
  try {
    text = (await res.text()).trim();
  } catch {
    return new Error(status);
  }
  if (text.length === 0) return new Error(status);
  try {
    const parsed: unknown = JSON.parse(text);
    if (typeof parsed === "object" && parsed !== null && "error" in parsed) {
      const reason = (parsed as { error: unknown }).error;
      if (typeof reason === "string" && reason.length > 0) return new Error(`${status}: ${reason}`);
    }
  } catch {
    // Not JSON -- the plain-text body is the reason.
  }
  return new Error(`${status}: ${text.slice(0, 300)}`);
}

/**
 * One request path for every daemon call: throws on a non-2xx status and
 * validates the response body, so callers narrow from a parsed value rather
 * than from `any`.
 */
export async function requestJson<T>(
  path: string,
  schema: ZodType<T>,
  options: RequestOptions = {},
): Promise<T> {
  const init: RequestInit = { method: options.method ?? "GET" };
  if (options.body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(options.body);
  }
  if (options.signal) init.signal = options.signal;

  const res = await fetch(path, init);
  if (!res.ok) throw await refusal(res);
  const json: unknown = await res.json();
  return schema.parse(json);
}

/**
 * A request whose response body carries nothing the caller needs -- the body
 * save, which bumps no revision and returns the working copy we already hold.
 */
export async function requestOk(path: string, options: RequestOptions = {}): Promise<void> {
  const init: RequestInit = { method: options.method ?? "GET" };
  if (options.body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(options.body);
  }
  if (options.signal) init.signal = options.signal;

  const res = await fetch(path, init);
  if (!res.ok) throw await refusal(res);
}

export function failure(request: string, err: unknown): FetchFailure {
  return { request, message: describeError(err) };
}
