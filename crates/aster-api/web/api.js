// Same-origin API client. The browser holds an HttpOnly session cookie; writes
// carry the per-session CSRF token. The owner token is never stored.

let csrf = null;
let onUnauthorized = () => {};

export function setCsrf(token) { csrf = token; }
export function whenUnauthorized(fn) { onUnauthorized = fn; }

export class ApiError extends Error {
  constructor(status, code, detail) {
    super(detail ? `${code}: ${detail}` : code);
    this.status = status;
    this.code = code;
  }
}

export async function api(method, path, body) {
  const headers = { Accept: 'application/json' };
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (method !== 'GET' && csrf) headers['X-Aster-CSRF'] = csrf;
  let response;
  try {
    response = await fetch(path, {
      method,
      headers,
      credentials: 'same-origin',
      cache: 'no-store',
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch {
    throw new ApiError(0, 'offline', 'Aster could not be reached');
  }
  if (response.status === 204) return null;
  const text = await response.text();
  let data = text;
  const type = response.headers.get('content-type') || '';
  if (type.includes('json') && text) {
    try { data = JSON.parse(text); } catch { /* keep text */ }
  }
  if (!response.ok) {
    if (response.status === 401 && !path.startsWith('/v1/session')) onUnauthorized();
    const code = (data && data.error) || response.statusText || 'error';
    throw new ApiError(response.status, code, data && data.detail);
  }
  return data;
}

export const get = (path) => api('GET', path);
export const post = (path, body = {}) => api('POST', path, body);
export const patch = (path, body) => api('PATCH', path, body);
export const del = (path) => api('DELETE', path);

/** Live events with reconnect (backoff 0.5 s → 5 s) and a resync callback. */
export function stream({ onEvent, onState, onResync }) {
  let source = null;
  let delay = 500;
  let stopped = false;
  let everOpened = false;
  const open = () => {
    if (stopped) return;
    source = new EventSource('/v1/events');
    source.onopen = () => {
      delay = 500;
      onState(true);
      if (everOpened) onResync();
      everOpened = true;
    };
    source.onmessage = (message) => {
      try { onEvent(JSON.parse(message.data)); } catch { /* ignore malformed */ }
    };
    source.onerror = () => {
      source.close();
      onState(false);
      if (!everOpened) onResync();
      setTimeout(open, delay);
      delay = Math.min(delay * 2, 5000);
    };
  };
  open();
  return () => { stopped = true; if (source) source.close(); };
}
