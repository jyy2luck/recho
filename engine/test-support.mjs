// Development-only adapter: the application bundles and runs the Rust binary directly.
import { execFile, spawn } from 'node:child_process';
import assert from 'node:assert/strict';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { stat, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import http from 'node:http';

// Windows can allocate 10080 for port 0; fetch rejects that browser-restricted port.
export function httpFetch(url, options = {}) {
  return new Promise((resolve, reject) => {
    const request = http.request(url, { ...options, agent: false }, response => {
      const chunks = [];
      response.on('data', chunk => chunks.push(chunk));
      response.once('error', reject);
      response.once('end', () => resolve(new Response(
        [204, 205, 304].includes(response.statusCode) ? null : Buffer.concat(chunks),
        { status: response.statusCode, headers: response.headers },
      )));
    });
    request.once('error', reject);
    request.setTimeout(15000, () => request.destroy(new Error('Local test HTTP request timed out')));
    if (options.body !== undefined && options.body !== null) request.setHeader('content-length', Buffer.byteLength(options.body));
    request.end(options.body);
  });
}

export const engineBinary = process.env.RECHO_ENGINE_BIN || fileURLToPath(new URL(`../app/build/debug/recho-engine${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));

export async function assertPrivateFile(file) {
  if (process.platform !== 'win32') {
    assert.equal((await stat(file)).mode & 0o777, 0o600);
    return;
  }
  const { stdout } = await promisify(execFile)('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', '(Get-Acl -LiteralPath $env:RECHO_TEST_ACL_PATH).Sddl'], {
    env: { ...process.env, RECHO_TEST_ACL_PATH: file }, windowsHide: true,
  });
  assert.match(stdout, /D:P/);
  const principals = [...stdout.matchAll(/;;;([^;)]+)\)/g)].map(match => match[1]);
  assert.deepEqual(new Set(principals), new Set(['OW', 'SY']));
}

export async function startEngine(options) {
  const environment = { ...process.env, RECHO_CONTROL_TOKEN: options.token, RECHO_DATA_DIR: options.dataDir, RECHO_PROXY_PORT: String(options.proxyPort ?? 0) };
  // Explicit null exercises saved/default ports without inheriting a test
  // runner's environment override. Existing tests keep isolated ephemeral ports.
  if (options.proxyPort === null) delete environment.RECHO_PROXY_PORT;
  for (const [key, name] of Object.entries({ maxPreviewBytes: 'MAX_PREVIEW_BYTES', maxRequestBytes: 'MAX_REQUEST_BYTES', maxSessions: 'MAX_SESSIONS', requestTimeoutMs: 'REQUEST_TIMEOUT_MS', breakpointTimeoutMs: 'BREAKPOINT_TIMEOUT_MS', scriptTimeoutMs: 'SCRIPT_TIMEOUT_MS' })) {
    if (options[key] !== undefined) environment[`RECHO_${name}`] = String(options[key]);
  }
  if (options.upstreamCA) {
    const location = path.join(options.dataDir, 'test-upstream-ca.pem');
    await writeFile(location, options.upstreamCA, { mode: 0o600 });
    environment.RECHO_UPSTREAM_CA = location;
  }
  const child = spawn(engineBinary, [], { env: environment, stdio: ['ignore', 'pipe', 'pipe'] });
  let stderr = '';
  child.stderr.on('data', chunk => { stderr += chunk; });
  let output = '';
  let timer;
  const startup = await new Promise((resolve, reject) => {
    timer = setTimeout(() => { child.kill('SIGKILL'); reject(new Error(`Rust engine startup timed out: ${stderr}`)); }, 20000);
    child.once('error', reject);
    child.once('exit', code => reject(new Error(`Rust engine exited ${code}: ${stderr}`)));
    child.stdout.on('data', chunk => {
      output += chunk;
      if (output.includes('\n')) {
        try { resolve(JSON.parse(output.trim())); } catch (error) { reject(error); }
      }
    });
  }).finally(() => clearTimeout(timer));
  return {
    ...startup,
    async close() {
      if (child.exitCode !== null || child.signalCode !== null) return;
      const exited = once(child, 'exit');
      child.kill('SIGTERM');
      const killTimer = setTimeout(() => child.kill('SIGKILL'), 5000);
      try {
        const [code, signal] = await exited;
        // Windows process.kill terminates rather than delivering POSIX signals.
        // Graceful Windows shutdown is covered by typed desktop lifecycle tests.
        if (process.platform !== 'win32' && (code !== 0 || signal)) throw new Error(`Rust engine shutdown failed: ${code}/${signal}: ${stderr}`);
      } finally { clearTimeout(killTimer); }
    },
  };
}
