import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, readFile, writeFile, stat } from 'node:fs/promises';
import { once } from 'node:events';
import { spawn } from 'node:child_process';
import http from 'node:http';
import http2 from 'node:http2';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { deflateRawSync, gzipSync, constants as zlibConstants } from 'node:zlib';
import { startEngine, engineBinary, assertPrivateFile, httpFetch as fetch } from './test-support.mjs';

async function fixture(t) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'recho-extensions-'));
  const token = 'extensions-isolated-test';
  let engine = await startEngine({ dataDir: directory, token });
  t.after(async () => { await engine.close(); await rm(directory, { recursive: true, force: true }); });
  async function api(route, method = 'GET', data) {
    const response = await fetch(`http://127.0.0.1:${engine.controlPort}${route}`, {
      method, headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
      body: data === undefined ? undefined : JSON.stringify(data),
    });
    return { status: response.status, value: await response.json() };
  }
  async function restart() {
    await engine.close();
    engine = await startEngine({ dataDir: directory, token });
    return engine;
  }
  return { directory, engine, api, restart };
}
async function listen(server) { server.listen(0, '127.0.0.1'); await once(server, 'listening'); return server.address().port; }

test('MCP capture-list filters persist, preserve traffic and do not retroactively filter captures', async t => {
  const { directory, engine, api, restart } = await fixture(t);
  const origin = http.createServer(async (req, res) => {
    const chunks = []; for await (const bytes of req) chunks.push(bytes);
    res.writeHead(201, { 'content-type': 'application/octet-stream' });
    res.end(Buffer.concat(chunks));
  });
  const port = await listen(origin); t.after(() => origin.close());
  const url = `http://127.0.0.1:${port}/flowhub/v1/slots:batchGet?client_id=Xp6vsxz_7IYVw2BB`;
  const token = (await api('/mcp-access', 'POST', { name: 'Display filters' })).value.tokens[0].token;
  const client = mcpClient(t, directory, token);
  async function call(name, args = {}) {
    const response = await client('tools/call', { name, arguments: args });
    assert.equal(response.result?.isError, false, JSON.stringify(response));
    return JSON.parse(response.result.content[0].text);
  }
  async function send(body, target = url, headers = {}) {
    const response = await fetch(`http://127.0.0.1:${engine.proxyPort}`, {
      method: 'POST', path: target, headers: { host: `127.0.0.1:${port}`, ...headers }, body,
    });
    assert.equal(response.status, 201);
    assert.deepEqual(Buffer.from(await response.arrayBuffer()), Buffer.from(body));
    const record = (await api('/state')).value.sessions[0];
    assert.equal(record.url, target);
    return record;
  }
  const matching = '{"id":"Activity-Center-anzhuo"}';
  const before = await send('before');
  const filter = await call('capture_list_filter_create', { url, body: { member: 'id', equals: 'Activity-Center-anzhuo' } });
  assert.equal(filter.enabled, true);
  assert.deepEqual((await call('capture_list_filter_list')).filters, [filter]);
  assert.equal((await api('/history/' + before.id)).value.captureListMatches, undefined);
  for (const [body, target, headers, expected] of [
    [matching, url, {}, true],
    [' { "other": 1, "id": "Activity-Center-anzhuo" } ', url, {}, true],
    [JSON.stringify({ padding: 'x'.repeat(70_000), id: 'Activity-Center-anzhuo' }), url, {}, true],
    [gzipSync(matching), url, { 'content-encoding': 'gzip' }, true],
    ['{"id":"other"}', url, {}, false],
    ['', url, {}, false],
    ['not json', url, {}, false],
    ['{"id":', url, {}, false],
    ['{}', url, {}, false],
    [matching, url + 'x', {}, false],
  ]) {
    const record = await send(body, target, headers);
    assert.equal(record.captureListMatches[filter.id], expected);
    const archived = await fetch(`http://127.0.0.1:${engine.controlPort}/history/${record.id}/body?phase=request`, {
      headers: { authorization: 'Bearer extensions-isolated-test' },
    });
    assert.deepEqual(Buffer.from(await archived.arrayBuffer()), Buffer.from(body));
  }
  const saved = await readFile(path.join(directory, 'capture-list-filters.json'));
  for (const args of [{ url, body: {} }, { url: 'bad', body: { member: 'id', equals: 1 } }, { url, body: { member: 'id', equals: 1, extra: true } }]) {
    assert.equal((await client('tools/call', { name: 'capture_list_filter_create', arguments: args })).result.isError, true);
    assert.deepEqual(await readFile(path.join(directory, 'capture-list-filters.json')), saved);
  }
  await assertPrivateFile(path.join(directory, 'capture-list-filters.json'));
  const sessions = (await api('/state')).value.sessions;
  assert.equal(sessions.length, 11);
  assert.equal((await call('capture_live_filter', { query: url })).length, 11);
  await call('capture_list_filter_set_enabled', { id: filter.id, enabled: false });
  assert.equal((await call('capture_list_filter_list')).filters[0].enabled, false);
  const disabled = await send(matching);
  assert.equal(disabled.captureListMatches, undefined);
  await restart();
  const after = mcpClient(t, directory, token);
  const restartCall = async (name, args) => {
    const response = await after('tools/call', { name, arguments: args });
    assert.equal(response.result.isError, false, JSON.stringify(response));
    return JSON.parse(response.result.content[0].text);
  };
  assert.equal((await restartCall('capture_list_filter_list', {})).filters[0].enabled, false);
  const savedCapture = await restartCall('capture_live_get_by_id', { id: sessions[0].id });
  assert.equal(savedCapture.captureListMatches[filter.id], false);
  await restartCall('capture_list_filter_set_enabled', { id: filter.id, enabled: true });
  assert.equal((await restartCall('capture_list_filter_list', {})).filters[0].enabled, true);
  await restartCall('capture_list_filter_delete', { id: filter.id });
  assert.deepEqual((await restartCall('capture_list_filter_list', {})).filters, []);
  assert.equal((await restartCall('capture_live_get_by_id', { id: savedCapture.id })).id, savedCapture.id);
});

test('WebSocket negotiates deflate, reassembles fragments and preserves relayed compressed bytes', async t => {
  const { api, engine } = await fixture(t);
  function compressed(text) {
    const bytes = deflateRawSync(Buffer.from(text), { finishFlush: zlibConstants.Z_SYNC_FLUSH });
    assert.deepEqual(bytes.subarray(-4), Buffer.from([0, 0, 255, 255]));
    return bytes.subarray(0, -4);
  }
  function frame(first, bytes, mask) {
    assert.ok(bytes.length < 126);
    return mask ? Buffer.concat([Buffer.from([first, 128 | bytes.length]), mask, bytes.map((value, i) => value ^ mask[i % 4])])
      : Buffer.concat([Buffer.from([first, bytes.length]), bytes]);
  }
  const serverText = 'compressed server message 你好 '.repeat(20);
  const serverBytes = compressed(serverText), split = Math.floor(serverBytes.length / 2);
  const serverWire = Buffer.concat([frame(0x41, serverBytes.subarray(0, split)), frame(0x89, Buffer.from('ping')), frame(0x80, serverBytes.subarray(split))]);
  const clientText = 'masked compressed client message'.repeat(10);
  const clientWire = frame(0xc1, compressed(clientText), Buffer.from([1, 2, 3, 4]));
  const sockets = new Set(); let receivedByOrigin = Buffer.alloc(0);
  const origin = http.createServer();
  origin.on('connection', socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
  origin.on('upgrade', (req, socket, head) => {
    socket.on('end', () => socket.end());
    const accept = createHash('sha1').update(req.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\nSec-WebSocket-Extensions: permessage-deflate; client_no_context_takeover; server_no_context_takeover\r\n\r\n`);
    receivedByOrigin = Buffer.concat([receivedByOrigin, head]);
    socket.on('data', bytes => { receivedByOrigin = Buffer.concat([receivedByOrigin, bytes]); });
    socket.write(serverWire);
  });
  const port = await listen(origin);
  t.after(() => { for (const socket of sockets) socket.destroy(); origin.close(); });
  const url = `http://127.0.0.1:${port}/compressed`;
  const request = http.request({ host: '127.0.0.1', port: engine.proxyPort, path: url, headers: {
    connection: 'Upgrade', upgrade: 'websocket', 'sec-websocket-version': '13',
    'sec-websocket-key': 'dGhlIHNhbXBsZSBub25jZQ==', 'sec-websocket-extensions': 'permessage-deflate',
  } });
  const upgraded = once(request, 'upgrade'); request.end();
  const [response, socket, head] = await upgraded; t.after(() => socket.destroy());
  assert.match(response.headers['sec-websocket-extensions'], /permessage-deflate/);
  let received = Buffer.from(head); socket.on('data', bytes => { received = Buffer.concat([received, bytes]); });
  socket.write(clientWire);
  let session;
  for (let count = 0; count < 200; count++) {
    session = (await api('/state')).value.sessions.find(value => value.url === url);
    if (session?.websocketFrames?.length >= 4 && received.length === serverWire.length && receivedByOrigin.length === clientWire.length) break;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  assert.deepEqual(received, serverWire); assert.deepEqual(receivedByOrigin, clientWire);
  const frames = session.websocketFrames;
  assert.equal(frames.find(value => value.direction === 'response' && value.messageFinal)?.messageText, serverText);
  assert.equal(frames.find(value => value.direction === 'request')?.messageText, clientText);
  assert.ok(frames.every(value => value.decodeError === undefined));
  const closed = once(socket, 'close');
  socket.end();
  await closed;
});

test('API scripts mutate actual outgoing request and return persisted tests and variables', async t => {
  const { api } = await fixture(t);
  let hits = 0;
  const origin = http.createServer(async (req, res) => {
    hits++; const data = []; for await (const chunk of req) data.push(chunk);
    res.writeHead(201, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ token: req.headers['x-token'], body: Buffer.concat(data).toString() }));
  });
  const port = await listen(origin); t.after(() => origin.close());
  const result = await api('/request', 'POST', {
    url: `http://127.0.0.1:${port}/echo`, method: 'POST', body: 'before', variables: { token: 'local-secret' },
    beforeScript: "request.headers['x-token']=environment.token; request.body='modified'; console.log('pre');",
    afterScript: "test('status',()=>assert(response.status===201)); test('body',()=>assert(JSON.parse(response.body).body==='modified')); test('failure',()=>assert(false,'intentional')); environment.next='saved'; console.log('post');",
  });
  assert.equal(result.status, 200); assert.equal(result.value.error, null);
  assert.deepEqual(JSON.parse(result.value.responseBody), { token: 'local-secret', body: 'modified' });
  assert.deepEqual(result.value.tests.map(v => v.passed), [true, true, false]);
  assert.deepEqual(result.value.scriptLogs, ['pre', 'post']); assert.equal(result.value.variables.next, 'saved');
  const history = await api('/history');
  assert.deepEqual(history.value.find(v => v.id === result.value.id).tests, result.value.tests);
  const invalid = await api('/request', 'POST', { url: `http://127.0.0.1:${port}/`, beforeScript: "throw new Error('before-failure')" });
  assert.equal(invalid.status, 400); assert.match(invalid.value.error, /before-failure/); assert.equal(hits, 1);
});

test('composer selects actual HTTP2 and keeps response rewrites', async t => {
  const { api } = await fixture(t);
  const origin = http2.createServer();
  origin.on('stream', (stream, headers) => { assert.equal(headers[':path'], '/h2'); stream.respond({ ':status': 203, 'content-type': 'text/plain' }); stream.end('original-h2'); });
  const port = await listen(origin); t.after(() => origin.close());
  const url = `http://127.0.0.1:${port}/h2`;
  assert.equal((await api('/rules', 'PUT', { rules: [{ id: 'response', match: { url }, response: { body: 'rewritten-h2' } }] })).status, 200);
  const result = await api('/request', 'POST', { url, protocol: 'HTTP/2' });
  assert.equal(result.value.error, null); assert.equal(result.value.status, 203);
  assert.equal(result.value.responseBody, 'rewritten-h2'); assert.equal(result.value.protocol, 'HTTP/2');
  assert.equal((await api('/request', 'POST', { url, protocol: 'HTTP/9' })).status, 400);
});

test('original Python API hooks preserve binary, share environment, replace response and archive the result', async t => {
  const { api, engine } = await fixture(t);
  let sent;
  const origin = http.createServer(async (req, res) => {
    const chunks = []; for await (const chunk of req) chunks.push(chunk);
    sent = { headers: req.headers, body: Buffer.concat(chunks) };
    res.writeHead(200, { 'content-type': 'application/octet-stream' }); res.end(Buffer.from([0, 254, 3]));
  });
  const port = await listen(origin); t.after(() => origin.close());
  const source = `from recho import *
def onRequest(context, request):
    assert request.body.isBinary
    request.body.binary(request.body.payload + b'\\x80')
    request.headers['x-token'] = context.env['token']
    context.shared = {'before': True}
    print('pre-python')
    return request
def onResponse(context, response):
    assert context.shared['before']
    assert response.body.payload == b'\\x00\\xfe\\x03'
    response.code = 202
    response.body = {'changed': True}
    response.headers['content-type'] = 'application/json'
    context.env['result'] = 'saved'
    print('post-python')
    return response
`;
  const result = await api('/request', 'POST', { url: `http://127.0.0.1:${port}/`, method: 'POST', bodyBase64: Buffer.from([0, 255, 1]).toString('base64'), pythonScript: source, variables: { token: 'python-token' } });
  assert.equal(result.status, 200); assert.equal(result.value.error, null);
  assert.deepEqual(sent.body, Buffer.from([0, 255, 1, 128])); assert.equal(sent.headers['x-token'], 'python-token');
  assert.equal(result.value.status, 202); assert.deepEqual(JSON.parse(result.value.responseBody), { changed: true });
  assert.equal(result.value.variables.result, 'saved'); assert.match(result.value.scriptLogs.join(''), /pre-python[\s\S]*post-python/);
  const history = (await api('/history')).value.find(item => item.id === result.value.id);
  assert.equal(history.status, 202); assert.deepEqual(JSON.parse(history.responseBody), { changed: true });
  const response = await fetch(`http://127.0.0.1:${engine.controlPort}/history/${result.value.id}/body?phase=response`, { headers: { authorization: 'Bearer extensions-isolated-test' } });
  assert.equal(response.status, 200); assert.deepEqual(JSON.parse(await response.text()), { changed: true });
});

test('MCP stdio authenticates through the shared endpoint and rejects stale rules', async t => {
  const { directory, engine, api } = await fixture(t);
  const descriptorPath = path.join(directory, 'control.json');
  const descriptor = JSON.parse(await readFile(descriptorPath, 'utf8'));
  assert.equal(descriptor.url, `http://127.0.0.1:${engine.controlPort}`);
  await assertPrivateFile(descriptorPath);
  const token = (await api('/mcp-access', 'POST', { name: 'Test client' })).value.tokens.at(-1).token;
  const child = spawn(engineBinary, ['--mcp', '--scope', 'all'], { env: { ...process.env, RECHO_DATA_DIR: directory, RECHO_TOKEN: token }, stdio: ['pipe', 'pipe', 'pipe'] });
  t.after(() => { if (child.exitCode === null) child.kill(); });
  let buffer = ''; let sequence = 0; const waiting = new Map();
  child.stdout.on('data', chunk => {
    buffer += chunk;
    while (buffer.includes('\n')) {
      const end = buffer.indexOf('\n'); const line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
      const value = JSON.parse(line); const handler = waiting.get(value.id); waiting.delete(value.id); handler?.(value);
    }
  });
  const send = (method, params) => new Promise((resolve, reject) => {
    const id = ++sequence; const timer = setTimeout(() => reject(new Error(`MCP timed out: ${method}`)), 5000);
    waiting.set(id, value => { clearTimeout(timer); resolve(value); });
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
  });
  const call = async (name, args = {}) => (await send('tools/call', { name, arguments: args })).result;
  assert.equal((await send('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'local-test', version: '1' } })).result.serverInfo.name, 'recho');
  child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) + '\n');
  assert.ok((await send('tools/list', {})).result.tools.some(v => v.name === 'rule_update'));
  const rule = { id: 'mcp-rule', name: 'local-mcp', match: { url: 'http://mcp.test/*' }, mock: { body: 'mcp-response' } };
  assert.equal((await call('rule_create', { rule })).isError, false);
  const request = await call('http_request', { url: 'http://mcp.test/ok' });
  assert.equal(request.isError, false); assert.equal(JSON.parse(request.content[0].text).responseBody, 'mcp-response');
  assert.equal((await call('rule_update', { id: 'mcp-rule', original: { ...rule, name: 'stale' }, rule })).isError, true);
  assert.equal((await call('settings_update', { settings: { breakpointEnabled: false } })).isError, false);
  assert.equal((await api('/settings')).value.breakpointEnabled, false);
  assert.equal((await call('capture_rewrite_set_enabled', { enabled: false })).isError, false);
  assert.equal((await api('/settings')).value.rewriteEnabled, false);
  assert.equal((await call('capture_rewrite_set_enabled', { enabled: true })).isError, false);
  assert.equal((await call('capture_rewrite_get_by_id', { id: 'mcp-rule' })).isError, false);
  assert.equal((await call('capture_live_set_enabled', { enabled: 'yes' })).isError, true);
  const exited = once(child, 'exit'); child.stdin.end(); assert.equal((await exited)[0], 0);
});

test('MCP HTTP shares tools, remains local and uncaptured, and works while capture is paused or rebound', async t => {
  const { engine, api } = await fixture(t);
  const token = (await api('/mcp-access', 'POST', { name: 'Test client' })).value.tokens.at(-1).token;
  assert.match(token, /^[a-f0-9]{32}$/);
  let port = engine.proxyPort, sequence = 0;
  const direct = (options, body) => new Promise((resolve, reject) => {
    const request = http.request({ host: '127.0.0.1', port, path: '/mcp', method: 'POST', headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' }, ...options }, response => {
      const chunks = []; response.on('data', chunk => chunks.push(chunk));
      response.on('end', () => resolve({ status: response.statusCode, body: Buffer.concat(chunks).toString() }));
    });
    request.on('error', reject); request.end(body);
  });
  const send = async (message, options = {}) => {
    const response = await fetch(`http://127.0.0.1:${port}/mcp${options.query ?? ''}`, {
      method: options.method ?? 'POST',
      headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json', accept: 'application/json, text/event-stream', ...options.headers },
      body: options.method ? undefined : typeof message === 'string' ? message : JSON.stringify(message),
    });
    const body = await response.text();
    return { status: response.status, allow: response.headers.get('allow'), value: body ? JSON.parse(body) : null };
  };
  const rpc = (method, params = {}) => send({ jsonrpc: '2.0', id: ++sequence, method, params });
  const call = async (name, args = {}) => (await rpc('tools/call', { name, arguments: args })).value.result;
  const initialized = await rpc('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'local-http-test', version: '1' } });
  assert.equal(initialized.status, 200); assert.equal(initialized.value.result.protocolVersion, '2025-06-18');
  const notification = await send({ jsonrpc: '2.0', method: 'notifications/initialized' });
  assert.equal(notification.status, 202); assert.equal(notification.value, null);
  const tools = (await rpc('tools/list')).value.result.tools;
  assert.equal(tools.length, 63); assert.ok(tools.some(tool => tool.name === 'rule_update'));
  assert.equal((await call('settings_update', { settings: { breakpointEnabled: false } })).isError, false);
  assert.equal((await api('/settings')).value.breakpointEnabled, false);
  assert.equal((await call('settings_get')).isError, false);
  const rule = { id: 'http-mcp-rule', name: 'HTTP MCP', match: { url: 'http://http-mcp.test/*' }, mock: { body: 'http-mcp-response' } };
  assert.equal((await call('rule_create', { rule })).isError, false);
  const response = await call('http_request', { url: 'http://http-mcp.test/ok' });
  assert.equal(response.isError, false); assert.equal(JSON.parse(response.content[0].text).responseBody, 'http-mcp-response');
  assert.equal((await call('rule_update', { id: rule.id, original: { ...rule, name: 'stale' }, rule })).isError, true);
  assert.equal((await send({ jsonrpc: '2.0', id: 1, method: 'ping' }, { headers: { authorization: 'Bearer extensions-isolated-test' } })).status, 401);
  assert.equal((await send({}, { headers: { origin: 'https://foreign.example' } })).status, 403);
  assert.equal((await direct({ headers: { host: `foreign.example:${port}`, authorization: `Bearer ${token}`, 'content-type': 'application/json' } }, '{}')).status, 403);
  const lanAddress = Object.values(os.networkInterfaces()).flat().find(address => address.family === 'IPv4' && !address.internal)?.address;
  if (lanAddress) {
    assert.equal((await direct({ host: lanAddress, headers: { host: `127.0.0.1:${port}`, authorization: `Bearer ${token}`, 'content-type': 'application/json' } }, '{}')).status, 403);
  }
  assert.equal((await send({}, { query: '?scope=unsupported' })).status, 400);
  assert.equal((await send({}, { query: '?scope=all&scope=minimal' })).status, 400);
  assert.equal((await send({}, { headers: { 'mcp-protocol-version': '2000-01-01' } })).status, 400);
  assert.equal((await send('{')).value.error.code, -32700);
  assert.equal((await send([])).value.error.code, -32600);
  assert.equal((await rpc('missing')).value.error.code, -32601);
  assert.equal((await rpc('tools/call', { name: 'settings_update', arguments: [] })).value.error.code, -32602);
  for (const method of ['GET', 'DELETE']) {
    const result = await send(undefined, { method }); assert.equal(result.status, 405); assert.equal(result.allow, 'POST');
  }
  assert.equal((await send(' '.repeat(20 * 1024 * 1024 + 1))).status, 413);
  assert.equal((await call('capture_live_set_enabled', { enabled: false })).isError, false);
  assert.equal(JSON.parse((await call('capture_live_status')).content[0].text).running, false);
  const origin = http.createServer((_request, response) => { assert.fail('Paused capture must not forward traffic'); response.end(); });
  const originPort = await listen(origin); t.after(() => origin.close());
  const paused = await direct({ path: `http://127.0.0.1:${originPort}/paused` });
  assert.equal(paused.status, 503);
  port = (await api('/proxy', 'POST', { port: 0 })).value.proxyPort;
  assert.notEqual(port, engine.proxyPort);
  assert.equal((await rpc('ping')).status, 200);
  assert.equal(JSON.parse((await call('capture_live_status')).content[0].text).running, false);
  assert.equal((await call('capture_live_set_enabled', { enabled: true })).isError, false);
  const sessions = (await api('/state')).value.sessions;
  assert.equal(sessions.length, 1); assert.equal(sessions[0].url, 'http://http-mcp.test/ok');
});

test('MCP token lists preserve legacy credentials and revoke only deleted clients across transports and restarts', async t => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'recho-mcp-token-'));
  const controlToken = 'mcp-private-control', legacy = '1234567890abcdef1234567890abcdef';
  await writeFile(path.join(directory, 'mcp-token.json'), JSON.stringify({ token: legacy }));
  let engine = await startEngine({ dataDir: directory, token: controlToken });
  t.after(async () => { await engine.close(); await rm(directory, { recursive: true, force: true }); });
  const access = async (method = 'GET', body, token = controlToken, suffix = '') => {
    const response = await fetch(`http://127.0.0.1:${engine.controlPort}/mcp-access${suffix}`, {
      method, headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' }, body,
    });
    return { status: response.status, value: await response.json() };
  };
  const ping = token => fetch(`http://127.0.0.1:${engine.proxyPort}/mcp`, { method: 'POST', headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'ping' }) });
  const migrated = (await access()).value.tokens;
  assert.equal(migrated.length, 1); assert.equal(migrated[0].name, '默认令牌'); assert.equal(migrated[0].token, legacy);
  let first = migrated[0];
  let second = (await access('POST', JSON.stringify({ name: 'Cursor' }))).value.tokens[1];
  assert.notEqual(second.token, legacy);
  assert.equal(second.lastUsedAt, undefined);
  let firstClient = mcpClient(t, directory, first.token), secondClient = mcpClient(t, directory, second.token);
  assert.ok((await firstClient('initialize')).result); assert.ok((await secondClient('initialize')).result);
  [first, second] = (await access()).value.tokens;
  assert.ok(Number.isFinite(Date.parse(first.lastUsedAt)));
  assert.ok(Number.isFinite(Date.parse(second.lastUsedAt)));
  const saveRemote = await fetch(`http://127.0.0.1:${engine.controlPort}/settings`, { method: 'PATCH', headers: { authorization: `Bearer ${controlToken}`, 'content-type': 'application/json' }, body: JSON.stringify({ mcpRemoteEnabled: true }) });
  assert.equal(saveRemote.status, 200);
  await assertPrivateFile(path.join(directory, 'mcp-token.json'));
  assert.equal((await access('GET', undefined, legacy)).status, 401);
  for (const body of ['{', 'null', '[]', '{}', '{"name":" "}', '{"name":"x","token":"injected"}']) {
    assert.equal((await access('POST', body)).status, 400);
    assert.deepEqual((await access()).value.tokens, [first, second]);
  }
  assert.equal((await access('PATCH', '{"name":"Codex"}', legacy, '/' + first.id)).status, 401);
  assert.equal((await access('DELETE', '{}', controlToken, '/missing')).status, 404);
  assert.equal((await access('PATCH', '{"name":"Codex"}', controlToken, '/' + first.id)).status, 200);
  assert.equal((await access()).value.tokens[0].token, legacy);
  assert.equal((await ping(legacy)).status, 200); assert.ok((await firstClient('ping')).result);
  await engine.close(); engine = await startEngine({ dataDir: directory, token: controlToken });
  assert.equal((await access()).value.tokens[0].name, 'Codex');
  assert.equal((await access()).value.tokens[0].lastUsedAt, first.lastUsedAt);
  assert.equal((await access()).value.remoteEnabled, true);
  assert.equal((await ping(legacy)).status, 200); assert.equal((await ping(second.token)).status, 200);
  firstClient = mcpClient(t, directory, first.token); secondClient = mcpClient(t, directory, second.token);
  assert.ok((await firstClient('initialize')).result); assert.ok((await secondClient('initialize')).result);
  assert.equal((await access('DELETE', '{}', controlToken, '/' + first.id)).status, 200);
  assert.equal((await ping(legacy)).status, 401); assert.equal((await ping(second.token)).status, 200);
  assert.match((await firstClient('tools/list')).error.message, /Unauthorized/);
  assert.ok((await secondClient('tools/list')).result);
  assert.equal((await access('DELETE', '{}', controlToken, '/' + second.id)).status, 200);
  await engine.close(); engine = await startEngine({ dataDir: directory, token: controlToken });
  assert.deepEqual((await access()).value.tokens, []);
  assert.equal((await ping(second.token)).status, 401);
});

function mcpClient(t, directory, token, args = []) {
  const child = spawn(engineBinary, ['--mcp', ...args], {
    env: { ...process.env, RECHO_DATA_DIR: directory, RECHO_TOKEN: token }, stdio: ['pipe', 'pipe', 'pipe'],
  });
  t.after(() => { if (child.exitCode === null) child.kill(); });
  let buffer = '', sequence = 0;
  const waiting = new Map();
  child.stdout.on('data', chunk => {
    buffer += chunk;
    while (buffer.includes('\n')) {
      const end = buffer.indexOf('\n'), value = JSON.parse(buffer.slice(0, end)); buffer = buffer.slice(end + 1);
      waiting.get(value.id)?.(value); waiting.delete(value.id);
    }
  });
  return (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++sequence, timer = setTimeout(() => { waiting.delete(id); reject(new Error(`MCP timed out: ${method}`)); }, 5000);
    waiting.set(id, value => { clearTimeout(timer); resolve(value); });
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
  });
}

test('MCP helper requires explicit credentials, validates options and enforces minimal/all selection', async t => {
  const { directory, api } = await fixture(t);
  for (const [token, args] of [['', []], ['dummy', ['--port', '0']], ['dummy', ['--scope', 'unknown']], ['dummy', ['--host', 'http://other/']], ['dummy', ['--port']], ['dummy', ['--unknown', 'x']]]) {
    const child = spawn(engineBinary, ['--mcp', ...args], { env: { ...process.env, RECHO_DATA_DIR: directory, RECHO_TOKEN: token }, stdio: ['ignore', 'pipe', 'pipe'] });
    let output = ''; child.stdout.on('data', chunk => { output += chunk; });
    const [code] = await once(child, 'exit'); assert.equal(code, 1); assert.equal(output, '');
  }
  const token = (await api('/mcp-access', 'POST', { name: 'Test client' })).value.tokens.at(-1).token;
  const invalid = mcpClient(t, directory, 'wrong-token');
  assert.match((await invalid('initialize')).error.message, /Unauthorized/);
  const minimal = mcpClient(t, directory, token);
  assert.equal((await minimal('initialize')).result.serverInfo.name, 'recho');
  const tools = (await minimal('tools/list')).result.tools;
  assert.ok(tools.length > 0 && tools.length < 63);
  assert.ok(tools.some(tool => tool.name === 'capture_live_status'));
  assert.ok(tools.some(tool => tool.name === 'history_get_by_id'));
  assert.equal(tools.some(tool => tool.name === 'capture_live_clear'), false);
  assert.equal((await minimal('tools/call', { name: 'capture_live_clear', arguments: {} })).error.code, -32602);
  const all = mcpClient(t, directory, token, ['--scope', 'all']);
  assert.equal((await all('tools/list')).result.tools.length, 63);
  assert.equal((await all(7)).error.code, -32600);
  assert.equal((await all('tools/call', [])).error.code, -32602);
  assert.equal((await all('tools/call', { name: 'settings_update', arguments: { settings: { mcpRemoteEnabled: true } } })).result.isError, true);
  assert.equal((await api('/settings')).value.mcpRemoteEnabled, false);
  const access = (await api('/mcp-access')).value.tokens;
  const rotated = (await api('/mcp-access', 'POST', { name: 'Replacement' })).value.tokens.at(-1).token;
  await api('/mcp-access/' + access.find(item => item.token === token).id, 'DELETE', {});
  assert.notEqual(rotated, token);
  assert.match((await all('tools/list')).error.message, /Unauthorized/);
  const current = mcpClient(t, directory, rotated);
  assert.ok((await current('tools/list')).result.tools.length > 0);
});

test('MCP helper selects a non-loopback target without its private files and remote revocation is immediate', async t => {
  const host = Object.values(os.networkInterfaces()).flat().find(address => address.family === 'IPv4' && !address.internal)?.address;
  if (!host) { t.skip('No non-loopback IPv4 interface available'); return; }
  const target = await fixture(t), other = await fixture(t);
  const token = (await target.api('/mcp-access', 'POST', { name: 'Remote client' })).value.tokens.at(-1).token;
  // A different running instance in this directory must not override the explicit target.
  const client = mcpClient(t, other.directory, token, ['--host', host, '--port', String(target.engine.proxyPort), '--scope', 'all']);
  assert.match((await client('initialize')).error.message, /Remote MCP access is disabled/);
  assert.equal((await target.api('/settings', 'PATCH', { mcpRemoteEnabled: true })).status, 200);
  assert.equal((await client('initialize')).result.serverInfo.name, 'recho');
  const noFiles = mcpClient(t, path.join(other.directory, 'does-not-exist'), token, ['--host', host, '--port', String(target.engine.proxyPort)]);
  assert.equal((await noFiles('initialize')).result.serverInfo.name, 'recho');
  const state = await client('tools/call', { name: 'capture_live_status', arguments: {} });
  assert.equal(JSON.parse(state.result.content[0].text).proxyPort, target.engine.proxyPort);
  assert.equal((await client('tools/call', { name: 'settings_update', arguments: { settings: { retentionDays: 3 } } })).result.isError, false);
  assert.equal((await target.api('/settings')).value.retentionDays, 3);
  assert.equal((await other.api('/settings')).value.retentionDays, 0);
  assert.equal((await client('tools/call', { name: 'settings_update', arguments: { settings: { mcpRemoteEnabled: false } } })).result.isError, true);
  const agent = new http.Agent({ keepAlive: true });
  t.after(() => agent.destroy());
  const direct = (method, route, headers = {}, body) => new Promise((resolve, reject) => {
    const request = http.request({ host, port: target.engine.proxyPort, path: route, method, agent, headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json', ...headers } }, response => {
      response.resume(); response.on('end', () => resolve(response.statusCode));
    });
    request.on('error', reject); request.end(body);
  });
  assert.equal(await direct('POST', '/mcp', { authorization: 'Bearer wrong' }, '{}'), 401);
  assert.equal(await direct('POST', '/mcp', { origin: 'https://foreign.example' }, '{}'), 403);
  assert.equal(await direct('POST', '/mcp', { host: `foreign.example:${target.engine.proxyPort}` }, '{}'), 403);
  assert.notEqual(await direct('GET', '/mcp-access'), 200);
  assert.equal((await target.api('/settings', 'PATCH', { mcpRemoteEnabled: false })).status, 200);
  assert.match((await client('tools/list')).error.message, /Remote MCP access is disabled/);
  assert.equal(await direct('POST', '/mcp', {}, JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'ping' })), 403);
  const local = mcpClient(t, target.directory, token);
  assert.equal((await local('initialize')).result.serverInfo.name, 'recho');
});

test('MCP works through a forwarded local port without enabling remote access', async t => {
  const { directory, engine, api } = await fixture(t);
  const sockets = new Set();
  const relay = net.createServer(socket => {
    const target = net.connect(engine.proxyPort, '127.0.0.1');
    for (const connection of [socket, target]) {
      sockets.add(connection); connection.on('close', () => sockets.delete(connection));
      connection.on('error', () => { socket.destroy(); target.destroy(); });
    }
    socket.pipe(target).pipe(socket);
  });
  const port = await listen(relay);
  t.after(() => { for (const socket of sockets) socket.destroy(); relay.close(); });
  const token = (await api('/mcp-access', 'POST', { name: 'Test client' })).value.tokens.at(-1).token;
  const client = mcpClient(t, directory, token, ['--host', '127.0.0.1', '--port', String(port)]);
  assert.equal((await client('initialize')).result.serverInfo.name, 'recho');
  const response = await client('tools/call', { name: 'capture_live_status', arguments: {} });
  assert.equal(JSON.parse(response.result.content[0].text).proxyPort, engine.proxyPort);
  assert.equal((await api('/settings')).value.mcpRemoteEnabled, false);
});
