// =============================================================================
// e2b-proxy.ts — Webhook proxy running on Sprite
// =============================================================================
//
// Receives Telegram webhook requests via Sprite's wake-on-request,
// resumes the E2B sandbox if paused, and forwards the request.
//
// Required env vars:
//   E2B_API_KEY       — E2B API key
//   E2B_SANDBOX_ID    — Target E2B sandbox ID
//
// Runs on port 8080 (Sprite's HTTP proxy routes HTTPS to it).
// =============================================================================

import { Sandbox } from "e2b";

const E2B_API_KEY = Bun.env.E2B_API_KEY;
const E2B_SANDBOX_ID = Bun.env.E2B_SANDBOX_ID;

if (!E2B_API_KEY || !E2B_SANDBOX_ID) {
  console.error("E2B_API_KEY and E2B_SANDBOX_ID must be set");
  process.exit(1);
}

const TARGET_PORT = 8080;

// Cache the target host to avoid SDK calls on every request
let cachedHost: string | null = null;

async function resumeAndGetHost(): Promise<string> {
  console.log(`[proxy] Resuming sandbox ${E2B_SANDBOX_ID}...`);
  const sandbox = await Sandbox.connect(E2B_SANDBOX_ID, {
    apiKey: E2B_API_KEY,
  });
  const host = sandbox.getHost(TARGET_PORT);
  cachedHost = host;
  console.log(`[proxy] Sandbox resumed, target: https://${host}`);
  return host;
}

async function forward(
  host: string,
  path: string,
  method: string,
  contentType: string,
  body: Uint8Array | undefined
): Promise<Response> {
  return fetch(`https://${host}${path}`, {
    method,
    headers: { "Content-Type": contentType },
    body,
  });
}

const server = Bun.serve({
  port: 8080,
  async fetch(req) {
    const path = new URL(req.url).pathname;
    const contentType =
      req.headers.get("Content-Type") || "application/json";

    // Buffer body so we can retry if needed
    const body =
      req.method !== "GET" && req.method !== "HEAD"
        ? new Uint8Array(await req.arrayBuffer())
        : undefined;

    // Fast path: try cached host first
    if (cachedHost) {
      try {
        const res = await forward(
          cachedHost,
          path,
          req.method,
          contentType,
          body
        );
        if (res.ok || res.status < 500) return res;
      } catch {
        // Sandbox likely paused — fall through to resume
      }
    }

    // Slow path: resume sandbox and retry
    try {
      const host = await resumeAndGetHost();

      // Give tinyclaw a moment to bind to port after resume
      await Bun.sleep(3000);

      const res = await forward(host, path, req.method, contentType, body);
      return res;
    } catch (err) {
      console.error(`[proxy] Failed to forward after resume:`, err);
      return new Response("proxy error", { status: 502 });
    }
  },
});

console.log(`[proxy] E2B webhook proxy listening on port ${server.port}`);
console.log(`[proxy] Target sandbox: ${E2B_SANDBOX_ID}`);
