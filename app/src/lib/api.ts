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
  if (!res.ok) {
    throw new Error(`request failed with status ${res.status}`);
  }
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
  if (!res.ok) {
    throw new Error(`request failed with status ${res.status}`);
  }
}

export function failure(request: string, err: unknown): FetchFailure {
  return { request, message: describeError(err) };
}
