import { after, before, test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import http2 from 'node:http2';
import { createHash, createPrivateKey, X509Certificate } from 'node:crypto';
import https from 'node:https';
import net from 'node:net';
import tls from 'node:tls';
import { once } from 'node:events';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm, stat, writeFile, readdir, mkdir, rename } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { gzipSync, deflateSync, brotliCompressSync } from 'node:zlib';
import { startEngine, engineBinary, assertPrivateFile } from './test-support.mjs';
import { createCertificateAuthority } from './test-certificates.mjs';

const token = 'local-test-token';
let engine;
let directory;
let origin;
let secureOrigin;
let ca;
let originHits = 0;

function listen(server) {
  server.listen(0, '127.0.0.1');
  return once(server, 'listening').then(() => server.address().port);
}

function request(options, body) {
  return new Promise((resolve, reject) => {
    const outgoing = http.request({ agent: false, ...options }, incoming => {
      const chunks = [];
      incoming.on('data', chunk => chunks.push(chunk));
      incoming.once('end', () => resolve({ status: incoming.statusCode, headers: incoming.headers, body: Buffer.concat(chunks) }));
      incoming.once('error', reject);
    });
    outgoing.once('error', reject);
    outgoing.end(body);
  });
}

async function api(route, method = 'GET', body, authorization = `Bearer ${token}`) {
  const result = await request({ host: '127.0.0.1', port: engine.controlPort, path: route, method, headers: { authorization, 'content-type': 'application/json' } }, body === undefined ? undefined : JSON.stringify(body));
  return { status: result.status, value: JSON.parse(result.body) };
}

async function tunnel(authority, proxy = engine) {
  const socket = net.connect(proxy.proxyPort, '127.0.0.1');
  await once(socket, 'connect');
  socket.write(`CONNECT ${authority} HTTP/1.1\r\nHost: ${authority}\r\n\r\n`);
  await new Promise((resolve, reject) => {
    let pending = Buffer.alloc(0);
    const onError = error => { socket.off('data', onData); reject(error); };
    const onData = chunk => {
      pending = Buffer.concat([pending, chunk]);
      const end = pending.indexOf('\r\n\r\n');
      if (end === -1) return;
      socket.off('data', onData);
      socket.off('error', onError);
      if (!pending.subarray(0, end).toString().startsWith('HTTP/1.1 200')) { socket.destroy(); reject(new Error(pending.toString())); return; }
      if (pending.length > end + 4) socket.unshift(pending.subarray(end + 4));
      resolve();
    };
    socket.on('data', onData);
    socket.once('error', onError);
  });
  return socket;
}

async function httpsThroughProxy(proxy = engine, trustedCA = ca) {
  const port = secureOrigin.address().port;
  const raw = await tunnel(`127.0.0.1:${port}`, proxy);
  const secure = tls.connect({ socket: raw, host: '127.0.0.1', ca: trustedCA });
  await once(secure, 'secureConnect');
  const chunks = [];
  secure.on('data', chunk => chunks.push(chunk));
  const ended = once(secure, 'end');
  secure.write(`GET /secure HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nConnection: close\r\n\r\n`);
  await ended;
  return Buffer.concat(chunks).toString();
}

before(async () => {
  directory = await mkdtemp(path.join(os.tmpdir(), 'recho-engine-test-'));
  const authority = await createCertificateAuthority(directory);
  ca = await readFile(authority.caPath);
  const certificate = await authority.certificateForHost('127.0.0.1');
  secureOrigin = https.createServer(certificate, (incoming, response) => response.end('local TLS payload'));
  await listen(secureOrigin);
  origin = http.createServer((incoming, response) => {
    originHits += 1;
    if (['/gzip', '/deflate', '/br'].includes(incoming.url)) {
      const encoding = incoming.url.slice(1);
      response.writeHead(200, { 'content-type': 'application/json', 'content-encoding': encoding });
      response.end(({ gzip: gzipSync, deflate: deflateSync, br: brotliCompressSync })[encoding]('{"compressed":true}'));
    } else if (incoming.url === '/binary-echo') {
      response.writeHead(200, { 'content-type': 'application/octet-stream' });
      incoming.pipe(response);
    } else if (incoming.url === '/headers') {
      response.setHeader('content-type', 'application/json');
      response.end(JSON.stringify(incoming.headers));
    } else if (incoming.url === '/binary') {
      response.end(Buffer.from([0, 1, 2, 3]));
    } else if (incoming.url === '/large') {
      response.setHeader('content-type', 'text/plain');
      response.end('x'.repeat(5000));
    } else {
      const chunks = [];
      incoming.on('data', chunk => chunks.push(chunk));
      incoming.on('end', () => {
        response.setHeader('content-type', 'application/json');
        response.setHeader('x-test-header', incoming.headers['x-custom'] || 'none');
        response.end(JSON.stringify({ method: incoming.method, path: incoming.url, body: Buffer.concat(chunks).toString() }));
      });
    }
  });
  await listen(origin);
  engine = await startEngine({ token, dataDir: directory, proxyPort: 0, maxPreviewBytes: 1024, maxRequestBytes: 4096, maxSessions: 10, upstreamCA: ca });
});

test('state revalidation tracks capture updates, settings, deletion and clear', async t => {
  const dataDir = await mkdtemp(path.join(os.tmpdir(), 'recho-state-etag-'));
  const instance = await startEngine({ token, dataDir, proxyPort: 0, maxSessions: 2 });
  t.after(async () => { await instance.close(); await rm(dataDir, { recursive: true, force: true }); });
  const control = (route, method = 'GET', body, extraHeaders = {}) => request({
    host: '127.0.0.1', port: instance.controlPort, path: route, method,
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json', ...extraHeaders },
  }, body === undefined ? undefined : JSON.stringify(body));
  let snapshot = await control('/state');
  assert.equal(snapshot.status, 200);
  assert.ok(snapshot.headers.etag);
  let tag = snapshot.headers.etag;
  let cursor = JSON.parse(snapshot.body).sessionCursor;
  const delta = value => control(`/state?cursor=${encodeURIComponent(value)}`);
  const unchanged = await control('/state', 'GET', undefined, { 'if-none-match': `"stale", ${tag.slice(2)}` });
  assert.equal(unchanged.status, 304);
  assert.equal(unchanged.body.length, 0);
  assert.equal(unchanged.headers.etag, tag);
  assert.equal((await control('/state', 'GET', undefined, { 'if-none-match': tag, authorization: 'Bearer wrong' })).status, 401);

  const response = await control('/request', 'POST', { method: 'POST', url: `http://127.0.0.1:${origin.address().port}/etag`, body: 'preserved-body' });
  assert.equal(response.status, 200);
  const session = JSON.parse(response.body);
  snapshot = await control('/state', 'GET', undefined, { 'if-none-match': tag });
  assert.equal(snapshot.status, 200);
  assert.notEqual(snapshot.headers.etag, tag);
  assert.equal(JSON.parse(snapshot.body).sessions.find(s => s.id === session.id).requestBody, 'preserved-body');
  let changes = JSON.parse((await delta(cursor)).body);
  assert.equal(changes.sessionsDelta, true);
  assert.deepEqual(changes.sessionIDs, [session.id]);
  assert.equal(changes.sessions[0].requestBody, 'preserved-body');
  cursor = changes.sessionCursor;
  for (const invalid of ['invalid', '00000000-0000-0000-0000-000000000000:0', `${cursor.split(':')[0]}:18446744073709551615`]) {
    const fallback = JSON.parse((await delta(invalid)).body);
    assert.notEqual(fallback.sessionsDelta, true);
    assert.equal(fallback.sessions[0].id, session.id);
  }
  tag = snapshot.headers.etag;

  assert.equal((await control('/settings', 'PATCH', { captureGroup: 'etag-group' })).status, 200);
  snapshot = await control('/state', 'GET', undefined, { 'if-none-match': tag });
  assert.equal(snapshot.status, 200);
  assert.equal(JSON.parse(snapshot.body).settings.captureGroup, 'etag-group');
  changes = JSON.parse((await delta(cursor)).body);
  assert.deepEqual(changes.sessions, []);
  assert.deepEqual(changes.sessionIDs, [session.id]);
  tag = snapshot.headers.etag;
  assert.equal((await control(`/history/${session.id}`, 'DELETE')).status, 200);
  snapshot = await control('/state', 'GET', undefined, { 'if-none-match': tag });
  assert.equal(snapshot.status, 200);
  assert.deepEqual(JSON.parse(snapshot.body).sessions, []);
  changes = JSON.parse((await delta(cursor)).body);
  assert.deepEqual(changes.sessions, []);
  assert.deepEqual(changes.sessionIDs, []);
  tag = snapshot.headers.etag;

  let finish;
  const streaming = http.createServer((incoming, response) => {
    response.writeHead(200, { 'content-type': 'text/plain' });
    response.write('first-');
    finish = () => response.end('last');
  });
  const streamingPort = await listen(streaming);
  t.after(() => { streaming.closeAllConnections(); streaming.close(); });
  const url = `http://127.0.0.1:${streamingPort}/in-flight`;
  const transfer = request({ host: '127.0.0.1', port: instance.proxyPort, path: url });
  async function waitForSession(predicate) {
    for (let attempt = 0; attempt < 200; attempt++) {
      const result = await control('/state');
      const item = JSON.parse(result.body).sessions.find(s => s.url === url);
      if (item && predicate(item)) return { result, item };
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    assert.fail('Capture did not reach the expected state');
  }
  const pending = await waitForSession(() => finish !== undefined);
  finish();
  assert.equal((await transfer).body.toString(), 'first-last');
  const completed = await waitForSession(item => item.responseBody === 'first-last');
  assert.equal(completed.item.id, pending.item.id);
  assert.notEqual(completed.result.headers.etag, pending.result.headers.etag);
  changes = JSON.parse((await delta(JSON.parse(pending.result.body).sessionCursor)).body);
  assert.equal(changes.sessions[0].id, completed.item.id);
  assert.equal(changes.sessions[0].responseBody, 'first-last');
  cursor = changes.sessionCursor;
  for (let i = 0; i < 2; i++) {
    assert.equal((await control('/request', 'POST', { url: `http://127.0.0.1:${origin.address().port}/evict/${i}` })).status, 200);
  }
  changes = JSON.parse((await delta(cursor)).body);
  assert.equal(changes.sessions.length, 2);
  assert.equal(changes.sessionIDs.length, 2);
  assert.ok(!changes.sessionIDs.includes(completed.item.id));
  cursor = changes.sessionCursor;
  tag = completed.result.headers.etag;
  assert.equal((await control('/sessions', 'DELETE')).status, 200);
  snapshot = await control('/state', 'GET', undefined, { 'if-none-match': tag });
  assert.equal(snapshot.status, 200);
  changes = JSON.parse((await delta(cursor)).body);
  assert.deepEqual(changes.sessions, []);
  assert.deepEqual(changes.sessionIDs, []);
  assert.equal((await control('/state', 'GET', undefined, { 'if-none-match': snapshot.headers.etag })).status, 304);
  assert.equal((await control('/state')).status, 200);
});

test('state cursors and validators distinguish engine runs at the same revision', async t => {
  const dataDir = await mkdtemp(path.join(os.tmpdir(), 'recho-state-restart-'));
  let instance = await startEngine({ token, dataDir, proxyPort: 0 });
  t.after(async () => { await instance.close(); await rm(dataDir, { recursive: true, force: true }); });
  const read = (route, tag) => request({
    host: '127.0.0.1', port: instance.controlPort, path: route,
    headers: { authorization: `Bearer ${token}`, ...(tag ? { 'if-none-match': tag } : {}) },
  });
  const before = await read('/state');
  const previous = JSON.parse(before.body);
  assert.equal(previous.sessionsRevision, 0);
  assert.equal(before.headers['cache-control'], 'no-store');
  await instance.close();
  instance = await startEngine({ token, dataDir, proxyPort: 0 });
  const after = await read(`/state?cursor=${encodeURIComponent(previous.sessionCursor)}`, before.headers.etag);
  assert.equal(after.status, 200);
  const current = JSON.parse(after.body);
  assert.equal(current.sessionsRevision, 0);
  assert.equal(current.sessionsDelta, false);
  assert.notEqual(current.sessionCursor, previous.sessionCursor);
  assert.notEqual(after.headers.etag, before.headers.etag);
  assert.deepEqual(current.sessionIDs, []);
});

test('revision state polling transfers changed records and tracks live eviction and deletion', async () => {
  const dataDir = await mkdtemp(path.join(os.tmpdir(), 'recho-state-delta-'));
  const local = await startEngine({ token, dataDir, maxSessions: 2, maxPreviewBytes: 65536 });
  let release;
  let started;
  const began = new Promise(resolve => { started = resolve; });
  const fixture = http.createServer((incoming, response) => {
    response.setHeader('content-type', 'text/plain');
    if (incoming.url === '/slow') {
      release = () => response.end('completed');
      started();
    } else response.end('x'.repeat(65536));
  });
  await listen(fixture);
  const call = async (route, method = 'GET', input) => {
    const result = await request({ host: '127.0.0.1', port: local.controlPort,
      path: route, method, headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' } },
    input === undefined ? undefined : JSON.stringify(input));
    assert.equal(result.status, 200);
    return JSON.parse(result.body);
  };
  try {
    const initial = await call('/state');
    assert.equal(initial.sessionsDelta, false);
    const pending = request({ host: '127.0.0.1', port: local.proxyPort,
      path: `http://127.0.0.1:${fixture.address().port}/slow` });
    await began;
    const inFlight = await call(`/state?since=${initial.sessionsRevision}`);
    assert.equal(inFlight.sessionsDelta, true);
    assert.equal(inFlight.sessions.length, 1);
    release();
    await pending;
    let completed;
    for (let i = 0; i < 100; i++) {
      completed = await call(`/state?since=${inFlight.sessionsRevision}`);
      if (completed.sessions[0]?.responseBody === 'completed') break;
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    assert.equal(completed.sessions[0].id, inFlight.sessions[0].id);
    assert.equal(completed.sessions[0].responseBody, 'completed');
    const send = n => call('/request', 'POST', { url: `http://127.0.0.1:${fixture.address().port}/${n}` });
    const first = await send(1);
    const before = await call('/state');
    const idle = await call(`/state?since=${before.sessionsRevision}`);
    assert.deepEqual(idle.sessions, []);
    assert.deepEqual(idle.sessionIDs, before.sessionIDs);
    assert.ok(JSON.stringify(idle).length < 10000);
    const second = await send(2);
    const delta = await call(`/state?since=${before.sessionsRevision}`);
    assert.deepEqual(delta.sessions.map(s => s.id), [second.id]);
    assert.equal(delta.sessions[0].responseBody.length, 65536);
    assert.deepEqual(delta.sessionIDs, [second.id, first.id]);
    const full = await call('/state');
    assert.equal(full.sessions.length, 2);
    assert.equal(full.sessions[1].responseBody.length, 65536);
    await call(`/history/${first.id}`, 'DELETE');
    const deleted = await call(`/state?since=${delta.sessionsRevision}`);
    assert.deepEqual(deleted.sessions, []);
    assert.deepEqual(deleted.sessionIDs, [second.id]);
    await call('/sessions', 'DELETE');
    const cleared = await call(`/state?since=${deleted.sessionsRevision}`);
    assert.deepEqual(cleared.sessionIDs, []);
    await send(3);
    const fallback = await call('/state?since=18446744073709551615');
    assert.equal(fallback.sessionsDelta, false);
    assert.equal(fallback.sessions.length, 1);
    const stale = await call('/state?since=0');
    assert.deepEqual(stale.sessions, fallback.sessions);
    await call('/settings', 'PATCH', { incognito: true });
    const metadata = await call(`/state?since=${stale.sessionsRevision}`);
    assert.deepEqual(metadata.sessions, []);
    assert.equal(metadata.settings.incognito, true);
    assert.equal(metadata.historyPersistent, false);
    const invalid = await request({ host: '127.0.0.1', port: local.controlPort,
      path: '/state?since=invalid', headers: { authorization: `Bearer ${token}` } });
    assert.equal(invalid.status, 400);
  } finally {
    release?.();
    fixture.closeAllConnections();
    await new Promise(resolve => fixture.close(resolve));
    await local.close();
    await rm(dataDir, { recursive: true, force: true });
  }
});

test('the LAN address accepts proxy traffic while the control API stays on loopback', async t => {
  const { value: state } = await api('/state');
  const address = state.proxyHost;
  assert.ok(net.isIPv4(address));
  assert.ok(state.proxyHosts.includes(address));
  assert.equal(new Set(state.proxyHosts).size, state.proxyHosts.length);
  const detected = Object.values(os.networkInterfaces()).flat();
  assert.ok(state.proxyHosts.every(host => net.isIPv4(host) && detected.some(item => item?.address === host)));
  if (address === '127.0.0.1') { t.skip('No eligible LAN IPv4 interface available'); return; }
  assert.ok(Object.values(os.networkInterfaces()).flat().some(item => item?.address === address && !item.internal));
  const result = await request({ host: address, port: engine.proxyPort, path: `http://127.0.0.1:${origin.address().port}/lan-proxy`, signal: AbortSignal.timeout(3000) });
  assert.equal(result.status, 200);
  assert.equal(JSON.parse(result.body).path, '/lan-proxy');
  await assert.rejects(request({ host: address, port: engine.controlPort, path: '/state', signal: AbortSignal.timeout(3000) }), error => ['ECONNREFUSED', 'ABORT_ERR'].includes(error.code));
});

test('deflate and Brotli preserve their wire bytes and support text rewrites', async t => {
  t.after(() => api('/rules', 'PUT', { rules: [] }));
  for (const encoding of ['deflate', 'br']) {
    const url = `http://127.0.0.1:${origin.address().port}/${encoding}`;
    const unchanged = await request({ host: '127.0.0.1', port: engine.proxyPort, path: url });
    assert.equal(unchanged.headers['content-encoding'], encoding);
    assert.equal((await api('/state')).value.sessions[0].responseBody, '{"compressed":true}');
    await api('/rules', 'PUT', { rules: [{ match: { url }, response: { replace: { search: 'true', replacement: 'false' } } }] });
    const changed = await request({ host: '127.0.0.1', port: engine.proxyPort, path: url });
    assert.equal(changed.headers['content-encoding'], undefined);
    assert.equal(changed.body.toString(), '{"compressed":false}');
    await api('/rules', 'PUT', { rules: [] });
  }
});

test('header-only rules preserve binary uploads and downloads', async t => {
  t.after(() => api('/rules', 'PUT', { rules: [] }));
  const url = `http://127.0.0.1:${origin.address().port}/binary-echo`;
  await api('/rules', 'PUT', { rules: [{ match: { url }, request: { headers: { 'x-binary': 'yes' } }, response: { headers: { 'x-response': 'yes' } } }] });
  const payload = Buffer.from([0, 255, 128, 1, 2, 3]);
  const result = await request({ host: '127.0.0.1', port: engine.proxyPort, path: url, method: 'POST', headers: { 'content-type': 'application/octet-stream' } }, payload);
  assert.deepEqual(result.body, payload);
  assert.equal(result.headers['x-response'], 'yes');
  const captured = (await api('/state')).value.sessions[0];
  assert.match(captured.requestBody, /Binary body: 6 bytes/);
  assert.equal(captured.requestBytes, 6);
});

test('ordinary uploads and downloads stream before their sender finishes', { timeout: 5000 }, async t => {
  let sawUpload;
  const firstUpload = new Promise(resolve => { sawUpload = resolve; });
  let finishDownload;
  const downloadReady = new Promise(resolve => { finishDownload = resolve; });
  const streaming = http.createServer((incoming, response) => {
    if (incoming.url === '/upload') {
      incoming.once('data', sawUpload);
      incoming.on('data', () => {});
      incoming.once('end', () => response.end('uploaded'));
    } else {
      response.write('first');
      downloadReady.then(() => response.end('last'));
    }
  });
  const port = await listen(streaming);
  t.after(() => { streaming.closeAllConnections(); streaming.close(); finishDownload(); });
  const outgoing = http.request({ agent: false, host: '127.0.0.1', port: engine.proxyPort, path: `http://127.0.0.1:${port}/upload`, method: 'POST' });
  const completed = once(outgoing, 'response').then(async ([response]) => { for await (const chunk of response) {} });
  outgoing.write('first upload');
  assert.equal((await firstUpload).toString(), 'first upload');
  outgoing.end('last upload');
  await completed;
  const download = http.request({ agent: false, host: '127.0.0.1', port: engine.proxyPort, path: `http://127.0.0.1:${port}/download` });
  download.end();
  const [response] = await once(download, 'response');
  const chunks = [];
  for await (const chunk of response) {
    chunks.push(chunk);
    if (chunks.length === 1) { assert.equal(chunk.toString(), 'first'); finishDownload(); }
  }
  assert.equal(Buffer.concat(chunks).toString(), 'firstlast');
});

test('fully received fixed-length responses finish successfully while partial disconnects remain errors', {timeout:5000}, async t=>{
  const origin=http.createServer((req,res)=>{
    if(req.url==='/partial'){res.writeHead(200,{'content-length':'10000'});res.write('partial');return;}
    const status=Number(req.url.slice(1));const body=status===204?'':`status ${status}`;
    res.writeHead(status,{'content-type':'text/plain','content-length':Buffer.byteLength(body)});res.end(body);
  });
  const port=await listen(origin);t.after(()=>{origin.closeAllConnections();origin.close();});
  for(const status of [200,501,204]){
    const url=`http://127.0.0.1:${port}/${status}`;
    const result=await request({host:'127.0.0.1',port:engine.proxyPort,path:url});assert.equal(result.status,status);
    const record=(await api('/state')).value.sessions.find(item=>item.url===url);
    assert.equal(record.error,null);assert.equal(record.responseBytes,result.body.length);assert.equal(record.responseBodyComplete,true);
  }
  const url=`http://127.0.0.1:${port}/partial`;
  const outgoing=http.request({host:'127.0.0.1',port:engine.proxyPort,path:url,agent:false});
  outgoing.on('error',()=>{});const response=once(outgoing,'response');outgoing.end();
  const [incoming]=await response;await once(incoming,'data');incoming.destroy();outgoing.destroy();
  let record;for(let i=0;i<100;i++){record=(await api('/state')).value.sessions.find(item=>item.url===url);if(record?.error)break;await new Promise(resolve=>setTimeout(resolve,10));}
  assert.match(record.error,/Client disconnected/);assert.equal(record.responseBytes,7);
});

test('ordered rewrite items execute all seven types on real traffic and share the preview implementation',async t=>{
  const origin=http.createServer(async(req,res)=>{const chunks=[];for await(const chunk of req)chunks.push(chunk);res.setHeader('content-type','application/json');res.end(JSON.stringify({url:req.url,body:Buffer.concat(chunks).toString(),header:req.headers['x-final'],removed:req.headers['x-drop']??null}));});
  const port=await listen(origin);t.after(async()=>{await api('/rules','PUT',{rules:[]});origin.closeAllConnections();origin.close();});
  const url=`http://127.0.0.1:${port}/ordered?raw=%2f&flag&drop=yes`;
  const rules=[{id:'ordered',match:{url:'*'},request:{mode:'modify',edits:[
    {type:'body',search:'ONE',replacement:'two',caseSensitive:false},
    {type:'queryAdd',replacement:'step=one'},
    {type:'headerAdd',replacement:'X-Added: one'},
    {type:'queryReplace',search:'step=(.*)',replacement:'step=two',regex:true},
    {type:'headerReplace',search:'X-Added: one',replacement:'X-Final: two'},
    {type:'queryRemove',search:'drop=yes'},
    {type:'headerRemove',search:'X-Drop: gone'}
  ]},response:{mode:'modify',edits:[{type:'body',search:'"body":"two"',replacement:'"body":"three"'},{type:'headerAdd',replacement:'X-Ordered: yes'}]}}];
  assert.equal((await api('/rules','PUT',{rules})).status,200);
  const reply=await request({host:'127.0.0.1',port:engine.proxyPort,path:url,method:'POST',headers:{'x-drop':'gone','content-type':'text/plain'}},'one');
  assert.equal(reply.status,200);assert.equal(reply.headers['x-ordered'],'yes');assert.deepEqual(JSON.parse(reply.body),{url:'/ordered?raw=%2f&flag&step=two',body:'three',header:'two',removed:null});
  const preview=await api('/rewrite/preview','POST',{edit:{type:'body',search:'ONE',replacement:'two',caseSensitive:false},data:'one ONE'});assert.equal(preview.value.result,'two two');
  const invalid=await api('/rewrite/preview','POST',{edit:{type:'body',search:'[',replacement:'x',regex:true},data:'one'});assert.equal(invalid.status,400);
  assert.equal((await api('/rules')).value.rules.length,1);
});

test('response breakpoints edit the response and disappear when a client disconnects', async t => {
  t.after(() => api('/rules', 'PUT', { rules: [] }));
  const url = `http://127.0.0.1:${origin.address().port}/response-breakpoint`;
  await api('/rules', 'PUT', { rules: [{ match: { url }, response: { breakpoint: true } }] });
  const reply = api('/request', 'POST', { url });
  let pending = [];
  for (let i = 0; i < 50 && !pending.length; i++) pending = (await api('/breakpoints')).value.pending;
  assert.equal(pending[0].phase, 'response');
  assert.equal((await api(`/breakpoints/${pending[0].id}`, 'POST', { action: 'continue', request: pending[0].request })).status, 400);
  assert.equal((await api(`/breakpoints/${pending[0].id}`, 'POST', { action: 'continue', response: { status: 203, headers: { 'x-edited': 'yes' }, body: 'edited reply' } })).status, 200);
  assert.equal((await reply).value.responseBody, 'edited reply');
  const outgoing = http.request({ agent: false, host: '127.0.0.1', port: engine.proxyPort, path: url });
  outgoing.on('error', () => {});
  outgoing.end();
  pending = [];
  for (let i = 0; i < 50 && !pending.length; i++) pending = (await api('/breakpoints')).value.pending;
  assert.equal(pending.length, 1);
  outgoing.destroy();
  let remaining = pending;
  for (let i = 0; i < 50 && remaining.length; i++) remaining = (await api('/breakpoints')).value.pending;
  assert.deepEqual(remaining, []);
  assert.equal((await api(`/breakpoints/${pending[0].id}`, 'POST', { action: 'continue' })).status, 404);
});

test('hop headers and proxy credentials are removed while Host is restored for the destination', async () => {
  const port = origin.address().port;
  const result = await request({ host: '127.0.0.1', port: engine.proxyPort, path: `http://127.0.0.1:${port}/headers`, headers: { connection: 'x-remove, host', 'x-remove': 'private', 'proxy-authorization': 'Basic test' } });
  const headers = JSON.parse(result.body);
  assert.equal(headers.host, `127.0.0.1:${port}`);
  assert.equal(headers['x-remove'], undefined);
  assert.equal(headers['proxy-authorization'], undefined);
  const recorded = (await api('/state')).value.sessions[0].requestHeaders;
  assert.doesNotMatch(recorded, /private|proxy-authorization/);
});

test('upstream idle timeouts return 504 and untrusted TLS origins are rejected', { timeout: 5000 }, async t => {
  const isolated = await mkdtemp(path.join(os.tmpdir(), 'recho-timeout-test-'));
  const timed = await startEngine({ token, dataDir: isolated, proxyPort: 0, requestTimeoutMs: 100 });
  const stalled = http.createServer(() => {});
  const port = await listen(stalled);
  t.after(async () => { stalled.closeAllConnections(); stalled.close(); await timed.close(); await rm(isolated, { recursive: true, force: true }); });
  const result = await request({ host: '127.0.0.1', port: timed.proxyPort, path: `http://127.0.0.1:${port}/timeout` });
  assert.equal(result.status, 504);
  const rejected = await request({ host: '127.0.0.1', port: timed.controlPort, path: '/request', method: 'POST', headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' } }, JSON.stringify({ url: `https://127.0.0.1:${secureOrigin.address().port}/untrusted` }));
  const session = JSON.parse(rejected.body);
  assert.equal(session.status, 0);
  assert.match(session.error, /certificate|UnknownIssuer/i);
});

test('request breakpoints may wait longer than network idle timeouts in composer and decrypted TLS', { timeout: 5000 }, async t => {
  const isolated = await mkdtemp(path.join(os.tmpdir(), 'recho-pause-test-'));
  const paused = await startEngine({ token, dataDir: isolated, proxyPort: 0, requestTimeoutMs: 100, breakpointTimeoutMs: 1500, upstreamCA: ca });
  t.after(async () => { await paused.close(); await rm(isolated, { recursive: true, force: true }); });
  const localAPI = async (route, method = 'GET', body) => {
    const result = await request({ host: '127.0.0.1', port: paused.controlPort, path: route, method, headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' } }, body === undefined ? undefined : JSON.stringify(body));
    assert.equal(result.status, 200, result.body.toString());
    return JSON.parse(result.body);
  };
  const url = `https://127.0.0.1:${secureOrigin.address().port}/secure`;
  await localAPI('/rules', 'PUT', { rules: [{ match: { url }, request: { breakpoint: true } }] });
  await localAPI('/proxy', 'POST', { interceptTLS: true });
  const trustedCA = await readFile(paused.caPath);
  for (const source of ['composer', 'tls']) {
    const reply = source === 'composer' ? localAPI('/request', 'POST', { url }) : httpsThroughProxy(paused, trustedCA);
    reply.catch(() => {});
    let pending = [];
    for (let i = 0; i < 100 && !pending.length; i++) {
      pending = (await localAPI('/breakpoints')).pending;
      if (!pending.length) await new Promise(resolve => setTimeout(resolve, 5));
    }
    assert.equal(pending.length, 1, `${source}: ${JSON.stringify(await localAPI('/state'))}`);
    await new Promise(resolve => setTimeout(resolve, 200));
    assert.equal((await localAPI('/breakpoints')).pending.length, 1);
    await localAPI(`/breakpoints/${pending[0].id}`, 'POST', { action: 'continue' });
    const result = await reply;
    assert.match(source === 'composer' ? result.responseBody : result, /local TLS payload/);
  }
});

after(async () => {
  await engine?.close();
  await Promise.all([origin, secureOrigin].filter(Boolean).map(server => new Promise(resolve => server.close(resolve))));
  if (directory) await rm(directory, { recursive: true, force: true });
});

test('the loopback control API requires the bearer token and exposes its actual ports', async () => {
  assert.equal((await api('/health', 'GET', undefined, '')).status, 401);
  assert.deepEqual((await api('/health')).value, { ok: true });
  const { value } = await api('/state');
  assert.equal(value.running, true);
  assert.equal(value.interceptTLS, false);
  assert.equal(value.proxyPort, engine.proxyPort);
  assert.equal(value.historyPersistent, true);
  await assertPrivateFile(path.join(directory, 'certificates', 'ca.key'));
});

test('composer executes a real HTTP request and records its request and response', async () => {
  const result = await api('/request', 'POST', { method: 'POST', url: `http://127.0.0.1:${origin.address().port}/echo?q=1`, headers: 'X-Custom: yes\nContent-Type: text/plain', body: 'hello world' });
  assert.equal(result.status, 200);
  assert.equal(result.value.status, 200);
  assert.equal(result.value.source, 'composer');
  assert.equal(result.value.application, undefined);
  assert.equal(result.value.requestBytes, 11);
  assert.equal(result.value.requestBody, 'hello world');
  assert.deepEqual(JSON.parse(result.value.responseBody), { method: 'POST', path: '/echo?q=1', body: 'hello world' });
  assert.match(result.value.responseHeaders, /x-test-header: yes/i);
});

test('SOCKS5 on the proxy port captures HTTP and persists the connected server IP', { timeout: 5000 }, async t => {
  const socket = net.connect(engine.proxyPort, '127.0.0.1');
  t.after(() => socket.destroy());
  await once(socket, 'connect');
  const read = async size => {
    let chunk;
    while ((chunk = socket.read(size)) === null) await once(socket, 'readable');
    return chunk;
  };
  socket.write(Buffer.from([5, 1, 0]));
  assert.deepEqual(await read(2), Buffer.from([5, 0]));
  const port = origin.address().port;
  socket.write(Buffer.from([5, 1, 0, 1, 127, 0, 0, 1, port >> 8, port & 255]));
  assert.equal((await read(10))[1], 0);
  socket.write(`GET /socks-server-ip HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nConnection: close\r\n\r\n`);
  let response = '';
  for await (const chunk of socket) response += chunk.toString();
  assert.match(response, /^HTTP\/1.1 200/);
  assert.match(response, /socks-server-ip/);
  const { value: state } = await api('/state');
  const captured = state.sessions.find(s => s.url.endsWith('/socks-server-ip'));
  assert.ok(captured);
  assert.equal(captured.serverIP, '127.0.0.1');
  const { value: archived } = await api(`/history/${captured.id}`);
  assert.equal(archived.serverIP, '127.0.0.1');
});

test('HTTP forward proxy preserves compressed bytes and decodes only the stored preview', async () => {
  const result = await request({ host: '127.0.0.1', port: engine.proxyPort, path: `http://127.0.0.1:${origin.address().port}/gzip` });
  assert.equal(result.status, 200);
  assert.equal(result.headers['content-encoding'], 'gzip');
  assert.deepEqual(result.body, gzipSync('{"compressed":true}'));
  const session = (await api('/state')).value.sessions[0];
  assert.equal(session.source, 'proxy');
  assert.equal(session.responseBody, '{"compressed":true}');
  assert.equal(session.responseBytes, result.body.length);
  if (process.platform === 'darwin') {
    assert.equal(session.application.pid, process.pid, 'attribute the client, not the engine');
    assert.ok(session.application.path.endsWith('/node'));
  }
});

test('previews disclose truncation and binary bodies while forwarding the full response', async () => {
  const large = await request({ host: '127.0.0.1', port: engine.proxyPort, path: `http://127.0.0.1:${origin.address().port}/large` });
  assert.equal(large.body.length, 5000);
  assert.match((await api('/state')).value.sessions[0].responseBody, /Preview truncated at 1024 bytes; 5000 bytes transferred/);
  await request({ host: '127.0.0.1', port: engine.proxyPort, path: `http://127.0.0.1:${origin.address().port}/binary` });
  assert.match((await api('/state')).value.sessions[0].responseBody, /Binary body: 4 bytes/);
});

test('CONNECT transports actual TLS without decrypting its payload by default', async () => {
  assert.match(await httpsThroughProxy(), /local TLS payload/);
  const session = (await api('/state')).value.sessions.find(item => item.method === 'CONNECT');
  assert.equal(session.status, 200);
  assert.equal(session.clientIP, '127.0.0.1');
  assert.match(session.responseBody, /Encrypted tunnel/);
  assert.ok(session.responseBytes > 0);
  if (process.platform === 'darwin') assert.equal(session.application.pid, process.pid);
});

test('optional MITM presents the local CA and records the decrypted HTTPS request', async () => {
  assert.equal((await api('/proxy', 'POST', { interceptTLS: true })).status, 200);
  assert.match(await httpsThroughProxy(), /local TLS payload/);
  const sessions = (await api('/state')).value.sessions;
  const decrypted = sessions.find(item => item.url === `https://127.0.0.1:${secureOrigin.address().port}/secure`);
  assert.ok(decrypted);
  assert.equal(decrypted.tls, true);
  assert.equal(decrypted.clientIP, '127.0.0.1');
  assert.equal(decrypted.status, 200);
  assert.equal(decrypted.responseBody, 'local TLS payload');
  if (process.platform === 'darwin') assert.equal(decrypted.application.pid, process.pid);
  await api('/proxy', 'POST', { interceptTLS: false });
});

test('invalid headers, URLs, setting types and oversized request bodies fail explicitly', async () => {
  const url = `http://127.0.0.1:${origin.address().port}/echo`;
  assert.equal((await api('/request', 'POST', { url, headers: 'broken' })).status, 400);
  assert.equal((await api('/request', 'POST', { url: 'file:///etc/passwd' })).status, 400);
  assert.equal((await api('/proxy', 'POST', { interceptTLS: 'yes' })).status, 400);
  assert.equal((await api('/request', 'POST', { url, body: 'x'.repeat(4097) })).status, 413);
  const rejected = await request({ host: '127.0.0.1', port: engine.proxyPort, path: url, method: 'POST', headers: { 'content-length': 4097 } }, 'x'.repeat(4097));
  assert.equal(rejected.status, 413);
  const failed = await api('/request', 'POST', { url: 'http://127.0.0.1:1/' });
  assert.equal(failed.value.status, 0);
  assert.match(failed.value.error, /ECONNREFUSED/);
  await assert.rejects(tunnel('127.0.0.1:1'), /502/);
  assert.match((await api('/state')).value.sessions[0].error, /ECONNREFUSED/);
});

test('rules rewrite streamed request and response bodies after a URL redirect', async t => {
  t.after(() => api('/rules', 'PUT', { rules: [] }));
  const base = `http://127.0.0.1:${origin.address().port}`;
  const saved = await api('/rules', 'PUT', { rules: [{ match: { url: `${base}/rule*` }, request: { redirect: `${base}/echo`, body: 'rewrite', headers: { 'x-custom': 'rule' } }, response: { status: 201, headers: { 'x-rule': 'applied' }, replace: { search: 'rewrite', replacement: 'output' } } }] });
  assert.equal(saved.status, 200);
  assert.equal((await api('/rules')).value.rules.length, 1);
  const result = await request({ host: '127.0.0.1', port: engine.proxyPort, path: `${base}/rule`, method: 'POST', headers: { 'content-type': 'text/plain' } }, 'input');
  assert.equal(result.status, 201);
  assert.equal(result.headers['x-rule'], 'applied');
  assert.equal(result.headers['x-test-header'], 'rule');
  assert.deepEqual(JSON.parse(result.body), { method: 'POST', path: '/echo', body: 'output' });
  const session = (await api('/state')).value.sessions[0];
  assert.equal(session.requestBody, 'rewrite');
  assert.equal(session.requestBytes, 7);
  assert.equal(session.responseBody, result.body.toString());
});

test('body rewrite decompresses gzip and fixes the forwarded encoding and content length', async t => {
  t.after(() => api('/rules', 'PUT', { rules: [] }));
  const url = `http://127.0.0.1:${origin.address().port}/gzip`;
  await api('/rules', 'PUT', { rules: [{ match: { url }, response: { replace: { search: 'true', replacement: 'false' } } }] });
  const result = await request({ host: '127.0.0.1', port: engine.proxyPort, path: url });
  assert.equal(result.headers['content-encoding'], undefined);
  assert.equal(result.body.toString(), '{"compressed":false}');
  assert.equal(Number(result.headers['content-length']), result.body.length);
});

test('a streamed body over the rule buffering limit receives an explicit HTTP error', async t => {
  t.after(() => api('/rules', 'PUT', { rules: [] }));
  const url = `http://127.0.0.1:${origin.address().port}/oversized-rule`;
  await api('/rules', 'PUT', { rules: [{ match: { url }, request: { body: 'replacement' } }] });
  const result = await request({ host: '127.0.0.1', port: engine.proxyPort, path: url, method: 'POST', headers: { 'transfer-encoding': 'chunked' } }, 'x'.repeat(4097));
  assert.equal(result.status, 413);
  assert.match(result.body.toString(), /Body buffering for rules exceeds 4096 bytes/);
  assert.match((await api('/state')).value.sessions[0].error, /4096/);
});

test('mock responses run response scripts without reaching the network', async t => {
  t.after(() => api('/rules', 'PUT', { rules: [] }));
  const url = 'https://not-a-real-local-target.invalid/mock';
  await api('/rules', 'PUT', { rules: [{ match: { url }, mock: { status: 202, body: 'mocked' }, response: { script: 'response.body = response.body.toUpperCase();' } }] });
  const result = await api('/request', 'POST', { url });
  assert.equal(result.value.status, 202);
  assert.equal(result.value.responseBody, 'MOCKED');
  assert.equal(result.value.error, null);
});

test('request breakpoints appear through the API and resume with edited content', async t => {
  t.after(() => api('/rules', 'PUT', { rules: [] }));
  const url = `http://127.0.0.1:${origin.address().port}/breakpoint`;
  await api('/rules', 'PUT', { rules: [{ match: { url }, request: { breakpoint: true } }] });
  const resultPromise = api('/request', 'POST', { method: 'POST', url, body: 'before' });
  let pending = [];
  for (let attempt = 0; attempt < 20 && !pending.length; attempt += 1) pending = (await api('/breakpoints')).value.pending;
  assert.equal(pending.length, 1);
  const path = `/breakpoints/${pending[0].id}`;
  const summaries = (await api('/breakpoints?summary=1')).value.pending;
  assert.equal(summaries.length, 1);
  assert.equal(summaries[0].id, pending[0].id);
  assert.deepEqual(summaries[0].request, { method: 'POST', url });
  assert.equal(summaries[0].response, undefined);
  const detail = await api(path);
  assert.deepEqual(detail.value, pending[0]);
  assert.equal((await api(path, 'POST', { action: 'continue', request: { ...detail.value.request, body: 'edited' } })).status, 200);
  const result = await resultPromise;
  assert.equal(result.value.requestBody, 'edited');
  assert.equal(JSON.parse(result.value.responseBody).body, 'edited');
  assert.equal((await api(path)).status, 404);
});

test('disconnecting a composer or proxy request cancels its breakpoint before any upstream request', async t => {
  t.after(() => api('/rules', 'PUT', { rules: [] }));
  const url = `http://127.0.0.1:${origin.address().port}/cancel-breakpoint`;
  await api('/rules', 'PUT', { rules: [{ match: { url }, request: { breakpoint: true } }] });
  for (const source of ['composer', 'proxy']) {
    const before = originHits;
    const outgoing = http.request({ agent: false, host: '127.0.0.1', port: source === 'composer' ? engine.controlPort : engine.proxyPort, path: source === 'composer' ? '/request' : url, method: 'POST', headers: source === 'composer' ? { authorization: `Bearer ${token}`, 'content-type': 'application/json' } : {} });
    outgoing.on('error', () => {});
    outgoing.end(source === 'composer' ? JSON.stringify({ url, method: 'POST', body: 'cancel' }) : 'cancel');
    let pending = [];
    for (let attempt = 0; attempt < 20 && !pending.length; attempt += 1) pending = (await api('/breakpoints')).value.pending;
    assert.equal(pending.length, 1);
    outgoing.destroy();
    let remaining = pending;
    for (let attempt = 0; attempt < 20 && remaining.length; attempt += 1) remaining = (await api('/breakpoints')).value.pending;
    assert.deepEqual(remaining, []);
    assert.equal((await api(`/breakpoints/${pending[0].id}`, 'POST', { action: 'continue' })).status, 404);
    assert.equal(originHits, before);
    assert.match((await api('/state')).value.sessions[0].error, /Client disconnected/);
  }
});

test('proxy can stop and restart on the same port, and sessions can be cleared', async () => {
  const port = engine.proxyPort;
  assert.equal((await api('/proxy', 'POST', { running: false })).value.running, false);
  assert.equal((await request({ host: '127.0.0.1', port, path: 'http://127.0.0.1/' })).status, 503);
  assert.equal((await api('/proxy', 'POST', { running: true })).value.proxyPort, port);
  await api('/sessions', 'DELETE');
  assert.deepEqual((await api('/state')).value.sessions, []);
});

test('proxy listening ports change atomically without losing history or in-flight breakpoints', async t => {
  const isolated = await mkdtemp(path.join(directory, 'proxy-port-'));
  const local = await startEngine({ token, dataDir: isolated, proxyPort: 0 });
  t.after(() => local.close());
  const localAPI = async (route, method = 'GET', body) => {
    const result = await request({ host: '127.0.0.1', port: local.controlPort, path: route, method, headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' } }, body === undefined ? undefined : JSON.stringify(body));
    return { status: result.status, value: JSON.parse(result.body) };
  };
  const freePort = async () => {
    const reservation = net.createServer();
    const port = await listen(reservation);
    await new Promise(resolve => reservation.close(resolve));
    return port;
  };
  const originalPort = local.proxyPort;
  const base = `http://127.0.0.1:${origin.address().port}`;
  await request({ host: '127.0.0.1', port: originalPort, path: `${base}/port-before` });
  const historyID = (await localAPI('/state')).value.sessions[0].id;
  const heldURL = `${base}/port-breakpoint`;
  await localAPI('/rules', 'PUT', { rules: [{ id: 'held-port-change', match: { url: heldURL }, request: { breakpoint: true } }] });
  const held = request({ host: '127.0.0.1', port: originalPort, path: heldURL, signal: AbortSignal.timeout(10_000) }).then(value => ({ value }), error => ({ error }));
  let pending = [];
  for (let attempt = 0; attempt < 100 && !pending.length; attempt++) pending = (await localAPI('/breakpoints')).value.pending;
  assert.equal(pending.length, 1);
  const newPort = await freePort();
  const changed = await localAPI('/proxy', 'POST', { port: newPort, interceptTLS: true });
  assert.equal(changed.status, 200);
  assert.equal(changed.value.proxyPort, newPort);
  assert.equal(changed.value.running, true);
  assert.equal(changed.value.interceptTLS, true);
  assert.ok(changed.value.sessions.some(session => session.id === historyID));
  assert.equal(changed.value.settings.proxyPort, newPort);
  await assert.rejects(request({ host: '127.0.0.1', port: originalPort, path: `${base}/old-port` }), /ECONNREFUSED/);
  assert.equal((await request({ host: '127.0.0.1', port: newPort, path: `${base}/new-port` })).status, 200);
  assert.equal((await localAPI('/breakpoints')).value.pending[0].id, pending[0].id);
  assert.equal((await localAPI(`/breakpoints/${pending[0].id}`, 'POST', { action: 'continue' })).status, 200);
  const completed = await held;
  assert.equal(completed.error, undefined);
  assert.equal(completed.value.status, 200);
  assert.equal(JSON.parse(completed.value.body).path, '/port-breakpoint');

  // A failed bind must also roll back other options supplied in the same call.
  const conflict = await localAPI('/proxy', 'POST', { port: origin.address().port, running: false, interceptTLS: false });
  assert.equal(conflict.status, 503);
  for (const port of [-1, 65536, 1.5, '9000', true, null, {}, []]) {
    assert.equal((await localAPI('/proxy', 'POST', { port, running: false, interceptTLS: false })).status, 400);
  }
  let current = (await localAPI('/state')).value;
  assert.equal(current.proxyPort, newPort);
  assert.equal(current.running, true);
  assert.equal(current.interceptTLS, true);
  assert.equal((await localAPI('/settings', 'PATCH', { proxyPort: 0 })).status, 400);

  // Force atomic settings replacement to fail, after the new socket was bound.
  const settingsFile = path.join(isolated, 'settings.json');
  const backup = settingsFile + '.backup';
  const unwrittenPort = await freePort();
  await rename(settingsFile, backup);
  await mkdir(settingsFile);
  try {
    assert.equal((await localAPI('/proxy', 'POST', { port: unwrittenPort, running: false, interceptTLS: false })).status, 502);
    current = (await localAPI('/state')).value;
    assert.equal(current.proxyPort, newPort);
    assert.equal(current.running, true);
    assert.equal(current.interceptTLS, true);
    assert.equal(current.settings.proxyPort, newPort);
    assert.equal((await request({ host: '127.0.0.1', port: newPort, path: `${base}/still-running` })).status, 200);
    const released = net.createServer();
    released.listen(unwrittenPort, '127.0.0.1');
    await once(released, 'listening');
    await new Promise(resolve => released.close(resolve));
  } finally {
    await rm(settingsFile, { recursive: true });
    await rename(backup, settingsFile);
  }
  assert.equal(JSON.parse(await readFile(settingsFile)).proxyPort, newPort);

  assert.equal((await localAPI('/proxy', 'POST', { running: false })).value.running, false);
  const pausedPort = await freePort();
  const paused = await localAPI('/proxy', 'POST', { port: pausedPort, interceptTLS: false });
  assert.equal(paused.status, 200);
  assert.equal(paused.value.proxyPort, pausedPort);
  assert.equal(paused.value.running, false);
  assert.equal(paused.value.interceptTLS, false);
  assert.equal((await request({ host: '127.0.0.1', port: pausedPort, path: base })).status, 503);
  assert.equal((await localAPI('/proxy', 'POST', { running: true })).value.proxyPort, pausedPort);
  assert.equal((await request({ host: '127.0.0.1', port: pausedPort, path: `${base}/resumed` })).status, 200);
  const random = await localAPI('/proxy', 'POST', { port: 0 });
  assert.equal(random.status, 200);
  assert.ok(random.value.proxyPort > 0);
  assert.notEqual(random.value.proxyPort, pausedPort);
  assert.equal(random.value.settings.proxyPort, 0);
  assert.equal((await request({ host: '127.0.0.1', port: random.value.proxyPort, path: `${base}/random-port` })).status, 200);
});

test('selected proxy ports persist while explicit environment ports take precedence', async t => {
  const isolated = await mkdtemp(path.join(directory, 'proxy-port-persist-'));
  let local = await startEngine({ token, dataDir: isolated, proxyPort: 0 });
  t.after(() => local.close());
  const reservation = net.createServer();
  const selectedPort = await listen(reservation);
  await new Promise(resolve => reservation.close(resolve));
  const changed = await request({ host: '127.0.0.1', port: local.controlPort, path: '/proxy', method: 'POST', headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' } }, JSON.stringify({ port: selectedPort }));
  assert.equal(changed.status, 200);
  await local.close();
  local = await startEngine({ token, dataDir: isolated, proxyPort: null });
  assert.equal(local.proxyPort, selectedPort);
  const url = `http://127.0.0.1:${origin.address().port}/persisted-port`;
  assert.equal((await request({ host: '127.0.0.1', port: selectedPort, path: url })).status, 200);
  await local.close();

  // Keep the saved port occupied: explicit 0 must still start independently.
  const occupied = net.createServer();
  occupied.listen(selectedPort, '127.0.0.1');
  await once(occupied, 'listening');
  try {
    local = await startEngine({ token, dataDir: isolated, proxyPort: 0 });
    assert.notEqual(local.proxyPort, selectedPort);
    assert.equal(JSON.parse(await readFile(path.join(isolated, 'settings.json'))).proxyPort, selectedPort);
    assert.equal((await request({ host: '127.0.0.1', port: local.proxyPort, path: url })).status, 200);
    await local.close();
  } finally { await new Promise(resolve => occupied.close(resolve)); }
  local = await startEngine({ token, dataDir: isolated, proxyPort: null });
  assert.equal(local.proxyPort, selectedPort);
});

test('HTTPS decryption survives restarts and disabled mode remains a transparent tunnel', async t => {
  const isolated = await mkdtemp(path.join(directory, 'tls-preference-'));
  const authority = await createCertificateAuthority(isolated);
  const localCA = await readFile(authority.caPath);
  let local = await startEngine({ token, dataDir: isolated, proxyPort: 0, upstreamCA: ca });
  t.after(() => local.close());
  const localAPI = async (route, method = 'GET', body) => {
    const response = await request({ host: '127.0.0.1', port: local.controlPort, path: route, method, headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' } }, body === undefined ? undefined : JSON.stringify(body));
    return { status: response.status, value: JSON.parse(response.body) };
  };
  assert.equal((await localAPI('/state')).value.interceptTLS, false);
  assert.equal((await localAPI('/proxy', 'POST', { interceptTLS: true })).value.interceptTLS, true);
  assert.equal((await localAPI('/settings', 'PATCH', { interceptTLS: false })).status, 400);
  assert.equal((await localAPI('/proxy', 'POST', { interceptTLS: 'yes' })).status, 400);
  assert.equal((await localAPI('/state')).value.interceptTLS, true);
  assert.equal(JSON.parse(await readFile(path.join(isolated, 'settings.json'))).interceptTLS, true);
  await local.close();
  local = await startEngine({ token, dataDir: isolated, proxyPort: 0, upstreamCA: ca });
  assert.equal((await localAPI('/state')).value.interceptTLS, true);
  assert.match(await httpsThroughProxy(local, localCA), /local TLS payload/);
  let sessions = (await localAPI('/state')).value.sessions;
  assert.ok(sessions.some(s => s.method === 'GET' && s.tls && s.status === 200));

  const settingsFile = path.join(isolated, 'settings.json');
  await rename(settingsFile, settingsFile + '.backup');
  await mkdir(settingsFile);
  try {
    assert.equal((await localAPI('/proxy', 'POST', { interceptTLS: false })).status, 502);
    assert.equal((await localAPI('/state')).value.interceptTLS, true);
  } finally {
    await rm(settingsFile, { recursive: true });
    await rename(settingsFile + '.backup', settingsFile);
  }
  assert.equal((await localAPI('/proxy', 'POST', { interceptTLS: false })).value.interceptTLS, false);
  await local.close();
  local = await startEngine({ token, dataDir: isolated, proxyPort: 0, upstreamCA: ca });
  assert.equal((await localAPI('/state')).value.interceptTLS, false);
  await localAPI('/sessions', 'DELETE');
  assert.match(await httpsThroughProxy(local, ca), /local TLS payload/);
  for (let attempt = 0; attempt < 100; attempt++) {
    sessions = (await localAPI('/state')).value.sessions;
    if (sessions.some(s => s.responseBody.includes('HTTPS decryption is disabled'))) break;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  assert.ok(sessions.every(s => s.method === 'CONNECT'));
  assert.ok(sessions.some(s => s.responseBody.includes('HTTPS decryption is disabled')));
});

test('SQLite retains older pages and restores history after a restart', async () => {
  await api('/sessions', 'DELETE');
  for (let index = 0; index < 13; index += 1) {
    await api('/request', 'POST', { url: `http://127.0.0.1:${origin.address().port}/history/${index}`,
      ...(index === 0 ? { method: 'POST', body: "literal '100%_ &+你好" } : {}) });
  }
  assert.equal((await api('/state')).value.sessions.length, 10);
  const newest = (await api('/history?offset=0&limit=10')).value;
  const older = (await api('/history?offset=10&limit=10')).value;
  assert.equal(newest.length, 10);
  assert.equal(older.length, 3);
  assert.match(newest[0].url, /\/history\/12$/);
  assert.match(older[2].url, /\/history\/0$/);
  assert.equal((await api('/history?offset=-1')).status, 400);
  const saved = older[2];
  assert.equal((await api('/state')).value.sessions.some(item => item.id === saved.id), false);
  assert.deepEqual((await api(`/history/${saved.id}`)).value, saved);
  assert.equal((await api(`/history/${saved.id}`, 'GET', undefined, '')).status, 401);
  assert.deepEqual((await api('/history?query=' + encodeURIComponent('/HISTORY/0'))).value, [saved]);
  assert.deepEqual((await api('/history?query=' + encodeURIComponent("literal '100%_ &+你好"))).value, [saved]);
  assert.deepEqual((await api('/history?query=' + encodeURIComponent("' OR 1=1 --"))).value, []);
  const filteredPage = (await api('/history?query=' + encodeURIComponent('/history/1') + '&offset=3&limit=1')).value;
  assert.equal(filteredPage.length, 1);
  assert.match(filteredPage[0].url, /\/history\/1$/);
  const access = (await api('/mcp-access', 'POST', { name: 'History lookup test' })).value.tokens.at(-1);
  const mcp = async (method, params) => {
    const result = await request({ host: '127.0.0.1', port: engine.proxyPort, path: '/mcp?scope=minimal', method: 'POST',
      headers: { authorization: `Bearer ${access.token}`, 'content-type': 'application/json' } },
      JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }));
    assert.equal(result.status, 200);
    return JSON.parse(result.body).result;
  };
  assert.ok((await mcp('tools/list')).tools.some(tool => tool.name === 'history_list'));
  const search = await mcp('tools/call', { name: 'history_list', arguments: { query: '/HISTORY/0', limit: 1 } });
  assert.equal(search.isError, false);
  assert.deepEqual(JSON.parse(search.content[0].text), [saved]);
  await engine.close();
  engine = await startEngine({ token, dataDir: directory, proxyPort: 0, maxSessions: 10, upstreamCA: ca });
  assert.deepEqual((await api('/history?offset=10')).value, older);
  assert.equal((await api('/state')).value.sessions.length, 0);
  const detail = await mcp('tools/call', { name: 'capture_live_get_by_id', arguments: { id: saved.id } });
  assert.equal(detail.isError, false);
  assert.deepEqual(JSON.parse(detail.content[0].text), saved);
  assert.equal((await api('/history/missing-id')).status, 404);
  await api(`/history/${saved.id}`, 'DELETE');
  assert.equal((await api(`/history/${saved.id}`)).status, 404);
  assert.equal((await mcp('tools/call', { name: 'capture_live_get_by_id', arguments: { id: saved.id } })).isError, true);
  await api(`/mcp-access/${access.id}`, 'DELETE', {});
  await api('/sessions', 'DELETE');
  assert.deepEqual((await api('/history')).value, []);
  assert.equal((await stat(path.join(directory, 'history.sqlite-wal'))).size, 0);
});

test('CLI prints one startup record and exits cleanly on SIGTERM', { timeout: 10_000, skip: process.platform === 'win32' ? 'POSIX signal; Windows lifecycle is tested through the embedded library' : false }, async t => {
  const child = spawn(engineBinary, [], { env: { ...process.env, RECHO_CONTROL_TOKEN: token, RECHO_DATA_DIR: path.join(directory, 'cli'), RECHO_PROXY_PORT: '0' }, stdio: ['ignore', 'pipe', 'pipe'] });
  t.after(() => { if (child.exitCode === null) child.kill('SIGKILL'); });
  const exited = once(child, 'exit');
  let output = '';
  const startup = await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.stdout.on('data', chunk => {
      output += chunk;
      if (output.includes('\n')) resolve(JSON.parse(output.trim()));
    });
    child.once('exit', code => reject(new Error(`CLI exited before startup with status ${code}`)));
  });
  assert.ok(startup.proxyPort > 0);
  assert.ok(startup.controlPort > 0);
  assert.ok(startup.caPath.endsWith('ca.pem'));
  const health = await request({ host: '127.0.0.1', port: startup.controlPort, path: '/health', headers: { authorization: `Bearer ${token}` } });
  assert.equal(health.status, 200);
  child.kill('SIGTERM');
  assert.deepEqual(await exited, [0, null]);
  assert.equal(output.trim().split('\n').length, 1);
  assert.equal(output.includes(token), false);
});

test('redirect uses the first match, rematches destination and applies replacement before modification', async t => {
  t.after(() => api('/rules', 'PUT', { rules: [] }));
  const base = `http://127.0.0.1:${origin.address().port}`;
  const rules = [
    { id:'modify', match:{url:`${base}/target*`}, request:{mode:'modify',query:[{name:'x',search:'^(.+)$',replacement:'p-$1',regex:true}]}, response:{mode:'modify',replace:{search:'(new)',replacement:'$1!',regex:true}} },
    { id:'redirect', match:{url:`${base}/source?`,exclude:[`${base}/source2`]}, request:{redirect:`${base}/target?keep=%2f&x=1&x=2`} },
    { id:'second', match:{url:`${base}/source*`}, request:{redirect:`${base}/wrong`} },
    { id:'destination-redirect', match:{url:`${base}/target*`}, request:{redirect:`${base}/also-wrong`} },
    { id:'replace', match:{url:`${base}/target*`}, request:{mode:'replace',method:'PUT',body:'replacement'},response:{mode:'replace',status:202,headers:{'content-type':'text/plain'},body:'new'} },
    { id:'original-only', match:{url:`${base}/source*`}, response:{body:'must not run'} },
  ];
  assert.equal((await api('/rules','PUT',{rules})).status,200);
  const result=await request({host:'127.0.0.1',port:engine.proxyPort,path:`${base}/source1`,method:'POST'},'old');
  assert.equal(result.status,202); assert.equal(result.body.toString(),'new!');
  const latest=(await api('/state')).value.sessions[0];
  assert.equal(latest.method,'PUT');assert.equal(latest.requestBody,'replacement');
  assert.equal(new URL(latest.url).pathname,'/target');assert.ok(latest.url.includes('keep=%2f'));
  assert.deepEqual(new URL(latest.url).searchParams.getAll('x'),['p-1','p-2']);
  const excluded=await request({host:'127.0.0.1',port:engine.proxyPort,path:`${base}/source2`});
  assert.equal(JSON.parse(excluded.body).path,'/wrong');
});

test('directory redirect preserves the full source path and optional Host', async t => {
  t.after(() => api('/rules', 'PUT', { rules: [] }));
  const base=`http://127.0.0.1:${origin.address().port}`;
  await api('/rules','PUT',{rules:[{match:{url:'http://original.invalid/*'},request:{redirect:`${base}/prefix/*`}}]});
  let result=await request({host:'127.0.0.1',port:engine.proxyPort,path:'http://original.invalid/foo/bar?q=ok'});
  assert.equal(JSON.parse(result.body).path,'/prefix/foo/bar?q=ok');
  await api('/rules','PUT',{rules:[{match:{url:'http://original.invalid/*'},request:{redirect:`${base}/headers`,preserveHost:true}}]});
  result=await request({host:'127.0.0.1',port:engine.proxyPort,path:'http://original.invalid/foo',headers:{host:'original.invalid'}});
  assert.equal(JSON.parse(result.body).host,'original.invalid');
});

test('binary local files replace request and response bodies without corruption', async t => {
  t.after(() => api('/rules','PUT',{rules:[]}));
  const base=`http://127.0.0.1:${origin.address().port}`;
  const filename=path.join(directory,'body.bin');const payload=Buffer.from([0,255,128,13,10,1,2]);await writeFile(filename,payload);
  let saved=await api('/rules','PUT',{rules:[{match:{url:`${base}/binary-echo`},request:{mode:'replace',bodyFile:filename}}]});
  assert.equal(saved.status,200);
  let result=await request({host:'127.0.0.1',port:engine.proxyPort,path:`${base}/binary-echo`,method:'POST'},Buffer.from([0,254]));
  assert.deepEqual(result.body,payload);assert.match((await api('/state')).value.sessions[0].requestHeaders,/content-length: 7/);
  await api('/rules','PUT',{rules:[{match:{url:`${base}/binary`},response:{mode:'replace',bodyFile:filename}}]});
  result=await request({host:'127.0.0.1',port:engine.proxyPort,path:`${base}/binary`});assert.deepEqual(result.body,payload);
  await api('/rules','PUT',{rules:[{match:{url:`${base}/binary`},response:{bodyFile:`${filename}.missing`}}]});
  assert.equal((await request({host:'127.0.0.1',port:engine.proxyPort,path:`${base}/binary`})).status,400);
});

test('category switches persist and compare-and-set rejects stale rule updates', async t => {
  t.after(async()=>{await api('/rules','PUT',{rules:[]});await api('/settings','PATCH',{rewriteEnabled:true,scriptEnabled:true,breakpointEnabled:true});});
  const base=`http://127.0.0.1:${origin.address().port}`;
  const initial=(await api('/rules')).value.rules;
  const saved=await api('/rules','PUT',{expected:initial,rules:[{id:'mixed',match:{url:`${base}/*`},request:{body:'rewritten',script:"request.headers['x-custom']='script';",breakpoint:true}}]});
  assert.equal(saved.status,200);assert.equal((await api('/rules','PUT',{expected:initial,rules:[]})).status,409);
  await api('/settings','PATCH',{rewriteEnabled:false,scriptEnabled:true,breakpointEnabled:false});
  const result=await request({host:'127.0.0.1',port:engine.proxyPort,path:`${base}/switch`,method:'POST'},'original');
  assert.equal(JSON.parse(result.body).body,'original');assert.equal(result.headers['x-test-header'],'script');
  assert.equal(JSON.parse(await readFile(path.join(directory,'settings.json'))).rewriteEnabled,false);
  await assertPrivateFile(path.join(directory,'settings.json'));
  assert.equal((await api('/settings','PATCH',{retentionDays:-1})).status,400);
});

test('single rule toggles use compare-and-set and reject invalid patches', async t => {
  t.after(() => api('/rules', 'PUT', { rules: [] }));
  const id = 'toggle / + ? 中文';
  const rule = { id, enabled: true, match: { url: '*' }, request: { script: "request.headers['x-toggle']='saved';" } };
  assert.equal((await api('/rules', 'PUT', { rules: [rule] })).status, 200);
  const initial = (await api('/rules')).value.rules[0];
  const toggled = await api('/rules/enabled', 'PATCH', { id, enabled: false, expected: true });
  assert.equal(toggled.status, 200);
  assert.deepEqual(toggled.value, { id, enabled: false });
  assert.deepEqual((await api('/rules')).value.rules[0], { ...initial, enabled: false });
  assert.equal((await api('/rules/enabled', 'PATCH', { id, enabled: true, expected: true })).status, 409);
  assert.equal((await api('/rules/enabled', 'PATCH', { id: 'missing', enabled: true, expected: false })).status, 404);
  assert.equal((await api('/rules/enabled', 'PATCH', { id, enabled: true, expected: false, request: {} })).status, 400);
  assert.equal((await api('/rules/enabled', 'PATCH', { id, enabled: 'true', expected: false })).status, 400);
  assert.equal((await api('/rules/enabled', 'PATCH', { enabled: true, expected: false })).status, 400);
  assert.equal((await api('/rules/enabled', 'PATCH', { id, enabled: true, expected: false })).status, 200);
});

test('history stores complete bodies beyond preview, supports decoding and individual deletion', async () => {
  const base=`http://127.0.0.1:${origin.address().port}`;
  const length=(await api('/state')).value.limits.maxPreviewBytes+512;
  const result=await api('/request','POST',{method:'POST',url:`${base}/echo`,body:'z'.repeat(length)});
  const id=result.value.id;assert.match(result.value.requestBody,/Preview truncated/);assert.equal(result.value.requestBodyComplete,true);
  let body=await request({host:'127.0.0.1',port:engine.controlPort,path:`/history/${id}/body?phase=request`,headers:{authorization:`Bearer ${token}`}});
  assert.equal(body.status,200);assert.equal(body.body.toString(),'z'.repeat(length));
  const compressed=await api('/request','POST',{url:`${base}/gzip`});
  body=await request({host:'127.0.0.1',port:engine.controlPort,path:`/history/${compressed.value.id}/body?decoded=true`,headers:{authorization:`Bearer ${token}`}});
  assert.equal(body.headers['content-encoding'],undefined);assert.equal(body.body.toString(),'{"compressed":true}');
  const { value: { sessions: [snapshot] } } = await api('/library/snapshots', 'POST', { ids: [id] });
  assert.equal(snapshot.retainedSnapshot, true);
  assert.equal(Buffer.from(snapshot.requestBodyBase64, 'base64').toString(), 'z'.repeat(length));
  assert.equal((await api(`/history/${id}`,'DELETE')).status,200);
  body=await request({host:'127.0.0.1',port:engine.controlPort,path:`/history/${id}/body?phase=request`,headers:{authorization:`Bearer ${token}`}});assert.equal(body.status,404);
});

test('incognito requests remain in memory without SQLite records or body files', async t => {
  t.after(()=>api('/settings','PATCH',{incognito:false,captureGroup:'default'}));
  await api('/settings','PATCH',{incognito:true,captureGroup:'private-test'});
  const result=await api('/request','POST',{url:`http://127.0.0.1:${origin.address().port}/private`,body:'private-body'});
  assert.equal(result.value.incognito,true);assert.equal(result.value.captureGroup,'private-test');
  assert.equal((await api('/library/snapshots', 'POST', { ids: [result.value.id] })).status, 400);
  assert.equal((await api('/history?limit=200')).value.some(item=>item.id===result.value.id),false);
  const files=await readdir(path.join(directory,'bodies'));assert.equal(files.some(name=>name.startsWith(result.value.id)),false);
  const body=await request({host:'127.0.0.1',port:engine.controlPort,path:`/history/${result.value.id}/body?phase=request`,headers:{authorization:`Bearer ${token}`}});assert.equal(body.body.toString(),'private-body');
  assert.deepEqual((await api(`/history/${result.value.id}`)).value,result.value);
  assert.deepEqual((await api('/history?query=private-body')).value,[]);
  for(let index=0;index<10;index++) await api('/request','POST',{url:`http://127.0.0.1:${origin.address().port}/private/${index}`});
  assert.equal((await api(`/history/${result.value.id}`)).status,404);
});

test('access profiles reject disallowed clients while authenticated local control remains reachable', async t => {
  t.after(()=>api('/settings','PATCH',{accessEnabled:false}));
  assert.equal((await api('/settings','PATCH',{accessProfiles:[{id:'deny-local',name:'test',mode:'deny',patterns:['127.0.0.?']}],activeAccessProfile:'deny-local',accessEnabled:true})).status,200);
  await assert.rejects(request({host:'127.0.0.1',port:engine.proxyPort,path:`http://127.0.0.1:${origin.address().port}/denied`}),error=>['ECONNRESET','EPIPE'].includes(error.code));
  assert.equal((await api('/state')).status,200);
  await api('/settings','PATCH',{accessProfiles:[{id:'local-cidr',name:'test',mode:'allow',patterns:['127.0.0.0/8']}],activeAccessProfile:'local-cidr'});
  assert.equal((await request({host:'127.0.0.1',port:engine.proxyPort,path:`http://127.0.0.1:${origin.address().port}/allowed`})).status,200);
});

test('gateway rules block before upstream and bypass capture without persisting bodies', async t => {
  t.after(()=>api('/settings','PATCH',{gatewayEnabled:false}));
  const base=`http://127.0.0.1:${origin.address().port}`;
  const gateways=[{id:'block',name:'block',enabled:true,layer:'l7',mode:'blockRequest',patterns:[`${base}/blocked`],showBlocked:false},{id:'bypass',name:'bypass',enabled:true,layer:'l7',mode:'bypass',patterns:[`${base}/bypass`]}];
  await api('/settings','PATCH',{gateways,gatewayEnabled:true});
  const before=originHits;const records=(await api('/history?limit=200')).value.length;const files=(await readdir(path.join(directory,'bodies'))).length;
  assert.equal((await request({host:'127.0.0.1',port:engine.proxyPort,path:`${base}/blocked`})).status,403);assert.equal(originHits,before);
  assert.equal((await request({host:'127.0.0.1',port:engine.proxyPort,path:`${base}/bypass`})).status,200);assert.equal(originHits,before+1);
  assert.equal((await api('/history?limit=200')).value.length,records);assert.equal((await readdir(path.join(directory,'bodies'))).length,files);
});

test('mirror maps host and port while Host policy remains selectable', async t => {
  t.after(()=>api('/settings','PATCH',{mirrorEnabled:false}));
  const mirrors=[{id:'mirror',name:'test',enabled:true,host:'mirror.test',target:`127.0.0.1:${origin.address().port}`,hostPolicy:'target',sniPolicy:'target'}];
  await api('/settings','PATCH',{mirrors,mirrorEnabled:true});
  let result=await request({host:'127.0.0.1',port:engine.proxyPort,path:'http://mirror.test/headers',headers:{host:'mirror.test'}});
  assert.equal(JSON.parse(result.body).host,`127.0.0.1:${origin.address().port}`);
  mirrors[0].hostPolicy='preserve';await api('/settings','PATCH',{mirrors});
  result=await request({host:'127.0.0.1',port:engine.proxyPort,path:'http://mirror.test/headers',headers:{host:'kept.test'}});assert.equal(JSON.parse(result.body).host,'kept.test');
});

test('reverse proxies forward requests and fail configuration atomically on occupied ports', async t => {
  t.after(()=>api('/settings','PATCH',{reverseEnabled:false,reverseProxies:[]}));
  const reservation=net.createServer();const port=await listen(reservation);await new Promise(resolve=>reservation.close(resolve));
  const reverseProxies=[{id:'reverse',name:'test',enabled:true,listenHost:'127.0.0.1',port,target:`http://127.0.0.1:${origin.address().port}`}];
  assert.equal((await api('/settings','PATCH',{reverseProxies,reverseEnabled:true})).status,200);
  let result=await request({host:'127.0.0.1',port,path:'/headers'});assert.equal(JSON.parse(result.body).host,`127.0.0.1:${origin.address().port}`);
  const rejected=await api('/settings','PATCH',{reverseProxies:[...reverseProxies,{id:'conflict',name:'conflict',enabled:true,listenHost:'127.0.0.1',port:origin.address().port,target:'http://example.test'}]});
  assert.equal(rejected.status,409);assert.equal((await api('/settings')).value.reverseProxies.length,1);
  result=await request({host:'127.0.0.1',port,path:'/still-running'});assert.equal(JSON.parse(result.body).path,'/still-running');
  for(let attempt=0;attempt<3;attempt++){
    assert.equal((await api('/settings','PATCH',{reverseEnabled:false})).status,200);
    assert.equal((await api('/settings','PATCH',{reverseEnabled:true})).status,200);
    result=await request({host:'127.0.0.1',port,path:'/restarted'});assert.equal(JSON.parse(result.body).path,'/restarted');
  }
});

test('SSL profiles selectively bypass interception and can hide bypassed tunnels', async t => {
  t.after(async()=>{await api('/settings','PATCH',{activeSSLProfile:null,sslProfiles:[]});await api('/proxy','POST',{interceptTLS:false});});
  await api('/proxy','POST',{interceptTLS:true});
  assert.equal((await api('/settings','PATCH',{sslProfiles:[{id:'bypass',name:'bypass',mode:'bypass',patterns:['127.0.0.1'],hideBypassed:true}],activeSSLProfile:'bypass'})).status,200);
  const before=(await api('/history?limit=200')).value.map(item=>item.id);
  assert.match(await httpsThroughProxy(),/local TLS payload/);
  const after=(await api('/history?limit=200')).value.map(item=>item.id);assert.deepEqual(after,before);
});

test('HTTP upstream supports absolute requests, Basic credentials, bypass and CONNECT TLS tunnels', async t => {
  const proxy=http.createServer((req,res)=>{res.setHeader('content-type','application/json');res.end(JSON.stringify({url:req.url,auth:req.headers['proxy-authorization']}));});
  const sockets=new Set();proxy.on('connection',socket=>{sockets.add(socket);socket.on('close',()=>sockets.delete(socket));});
  const expected=`Basic ${Buffer.from('tester:password').toString('base64')}`;
  let connects=0;
  proxy.on('connect',(req,client,head)=>{
    assert.equal(req.headers['proxy-authorization'],expected);connects++;
    const [host,port]=req.url.split(':');const upstream=net.connect(Number(port),host,()=>{client.write('HTTP/1.1 200 Connection Established\r\n\r\n');if(head.length)upstream.write(head);client.pipe(upstream);upstream.pipe(client);});
    upstream.on('error',()=>client.destroy());client.on('close',()=>upstream.destroy());
  });
  const port=await listen(proxy);
  t.after(async()=>{await api('/settings','PATCH',{upstreamEnabled:false});for(const socket of sockets)socket.destroy();await new Promise(resolve=>proxy.close(resolve));});
  const profiles=[{id:'upstream',name:'proxy',host:'127.0.0.1',port,mode:'allow',patterns:['*'],username:'tester',password:'password'}];
  assert.equal((await api('/settings','PATCH',{upstreamProfiles:profiles,activeUpstreamProfile:'upstream',upstreamEnabled:true})).status,200);
  const result=await request({host:'127.0.0.1',port:engine.proxyPort,path:'http://nonexistent.test/forwarded'});assert.deepEqual(JSON.parse(result.body),{url:'http://nonexistent.test/forwarded',auth:expected});
  await api('/proxy','POST',{interceptTLS:false});assert.match(await httpsThroughProxy(),/local TLS payload/);assert.equal(connects,1);
  profiles[0].mode='bypass';profiles[0].patterns=['127.0.0.1'];await api('/settings','PATCH',{upstreamProfiles:profiles});
  const direct=await request({host:'127.0.0.1',port:engine.proxyPort,path:`http://127.0.0.1:${origin.address().port}/direct`});assert.equal(JSON.parse(direct.body).path,'/direct');
});

test('binary API requests decode base64 exactly and reject conflicting or invalid bodies', async()=>{
  const payload=Buffer.from([0,255,128,1]);const url=`http://127.0.0.1:${origin.address().port}/binary-echo`;
  const result=await api('/request','POST',{url,method:'POST',bodyBase64:payload.toString('base64')});assert.equal(result.value.status,200);assert.equal(result.value.requestBytes,4);
  const body=await request({host:'127.0.0.1',port:engine.controlPort,path:`/history/${result.value.id}/body?phase=response`,headers:{authorization:`Bearer ${token}`}});assert.deepEqual(body.body,payload);
  assert.equal((await api('/request','POST',{url,body:'x',bodyBase64:'eA=='})).status,400);assert.equal((await api('/request','POST',{url,bodyBase64:'!!'})).status,400);
});

test('explicit HTTP2 uses authenticated upstream tunnels and mirrored authority', async t=>{
  const sessions=new Set();const sockets=new Set();const connected=[];
  const h2=http2.createServer();h2.on('session',session=>{sessions.add(session);session.on('close',()=>sessions.delete(session));});
  h2.on('stream',(stream,headers)=>{stream.respond({':status':200,'content-type':'application/json'});stream.end(JSON.stringify({authority:headers[':authority'],path:headers[':path']}));});
  const targetPort=await listen(h2);
  const upstream=http.createServer();
  const expected=`Basic ${Buffer.from('h2:local').toString('base64')}`;
  upstream.on('connection',socket=>{sockets.add(socket);socket.on('close',()=>sockets.delete(socket));});
  upstream.on('connect',(req,client,head)=>{
    assert.equal(req.headers['proxy-authorization'],expected);connected.push(req.url);
    const remote=net.connect(targetPort,'127.0.0.1',()=>{client.write('HTTP/1.1 200 Connection Established\r\n\r\n');if(head.length)remote.write(head);client.pipe(remote);remote.pipe(client);});
    sockets.add(remote);remote.on('close',()=>sockets.delete(remote));remote.on('error',()=>client.destroy());client.on('close',()=>remote.destroy());
  });
  const upstreamPort=await listen(upstream);
  t.after(async()=>{await api('/settings','PATCH',{upstreamEnabled:false,mirrorEnabled:false});for(const session of sessions)session.destroy();for(const socket of sockets)socket.destroy();upstream.close();h2.close();});
  const mirrors=[{id:'h2mirror',name:'h2mirror',host:'h2mirror.test',target:`127.0.0.1:${targetPort}`,hostPolicy:'target'}];
  assert.equal((await api('/settings','PATCH',{mirrors,mirrorEnabled:true,upstreamProfiles:[{id:'h2upstream',name:'h2upstream',host:'127.0.0.1',port:upstreamPort,mode:'allow',patterns:['*'],username:'h2',password:'local'}],activeUpstreamProfile:'h2upstream',upstreamEnabled:true})).status,200);
  const result=await api('/request','POST',{url:'http://h2mirror.test/through-upstream',protocol:'HTTP/2'});
  assert.equal(result.value.error,null);assert.equal(result.value.status,200);assert.equal(result.value.protocol,'HTTP/2');
  assert.deepEqual(JSON.parse(result.value.responseBody),{authority:`127.0.0.1:${targetPort}`,path:'/through-upstream'});
  assert.deepEqual(connected,[`127.0.0.1:${targetPort}`]);
  assert.doesNotMatch(result.value.requestHeaders,/proxy-authorization/i);
});

test('settings compare-and-set protects only touched fields from stale saves', async()=>{
  const settings=(await api('/settings')).value;
  assert.equal((await api('/settings','PATCH',{_expected:{rewriteEnabled:settings.rewriteEnabled},rewriteEnabled:!settings.rewriteEnabled})).status,200);
  assert.equal((await api('/settings','PATCH',{_expected:{rewriteEnabled:settings.rewriteEnabled},rewriteEnabled:settings.rewriteEnabled})).status,409);
  assert.equal((await api('/settings','PATCH',{_expected:{scriptEnabled:settings.scriptEnabled},scriptEnabled:settings.scriptEnabled})).status,200);
  await api('/settings','PATCH',{rewriteEnabled:settings.rewriteEnabled});
});

test('WebSocket upgrades relay masked and unmasked frames and expose a live frame history', async t=>{
  const server=http.createServer();const sockets=new Set();server.on('connection',socket=>{sockets.add(socket);socket.on('close',()=>sockets.delete(socket));});
  server.on('upgrade',(req,socket)=>{
    const accept=createHash('sha1').update(req.headers['sec-websocket-key']+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    socket.write(Buffer.from([0x81,5,...Buffer.from('hello')]));
    socket.once('data',()=>{socket.write(Buffer.from([0x82,3,0,128,255]));});socket.on('end',()=>socket.end());
  });
  const port=await listen(server);t.after(async()=>{for(const socket of sockets)socket.destroy();await new Promise(resolve=>server.close(resolve));});
  const req=http.request({host:'127.0.0.1',port:engine.proxyPort,path:`http://127.0.0.1:${port}/socket`,headers:{connection:'Upgrade',upgrade:'websocket','sec-websocket-version':'13','sec-websocket-key':'dGhlIHNhbXBsZSBub25jZQ=='}});
  const upgraded=once(req,'upgrade');req.end();const [response,socket,head]=await upgraded;assert.equal(response.statusCode,101);
  socket.on('data',()=>{});socket.write(Buffer.from([0x81,0x82,1,2,3,4,104^1,105^2].map(v=>typeof v==='string'?v.charCodeAt(0):v)));
  let recorded;
  for(let i=0;i<100;i++){recorded=(await api('/state')).value.sessions.find(s=>s.url===`http://127.0.0.1:${port}/socket`);if(recorded?.websocketFrames?.length>=3)break;await new Promise(resolve=>setTimeout(resolve,10));}
  assert.equal(recorded.protocol,'WebSocket');assert.equal(recorded.status,101);assert.equal(recorded.websocketFrames.find(frame=>frame.direction==='request').text,'hi');
  assert.ok(recorded.websocketFrames.some(frame=>frame.opcode===2&&frame.payloadBase64==='AID/'));
  socket.end();await once(socket,'close');
});

test('MITM negotiates HTTP2 end to end and preserves gRPC payloads and trailers', async t=>{
  const authority=await createCertificateAuthority(directory);const certificate=await authority.certificateForHost('127.0.0.1');
  const server=http2.createSecureServer(certificate);const connections=new Set();server.on('session',session=>{connections.add(session);session.on('close',()=>connections.delete(session));});
  const payload=Buffer.from([0,0,0,0,7,8,150,1,18,2,104,105]);
  server.on('stream',(stream,headers)=>{assert.equal(headers[':path'],'/service/Method');const input=[];stream.on('data',chunk=>input.push(chunk));stream.on('end',()=>{assert.deepEqual(Buffer.concat(input),payload);stream.respond({'content-type':'application/grpc'},{waitForTrailers:true});stream.on('wantTrailers',()=>stream.sendTrailers({'grpc-status':'0'}));stream.end(payload);});});
  const port=await listen(server);t.after(async()=>{await api('/proxy','POST',{interceptTLS:false});for(const session of connections)session.destroy();await new Promise(resolve=>server.close(resolve));});
  await api('/proxy','POST',{interceptTLS:true});
  const raw=await tunnel(`127.0.0.1:${port}`);const secure=tls.connect({socket:raw,host:'127.0.0.1',ca,ALPNProtocols:['h2']});await once(secure,'secureConnect');assert.equal(secure.alpnProtocol,'h2');
  const client=http2.connect(`https://127.0.0.1:${port}`,{createConnection:()=>secure});const outgoing=client.request({':method':'POST',':path':'/service/Method','content-type':'application/grpc','te':'trailers'});let trailers;outgoing.on('trailers',value=>{trailers=value;});const received=[];outgoing.on('data',chunk=>received.push(chunk));const ended=once(outgoing,'end');outgoing.end(payload);await ended;
  assert.deepEqual(Buffer.concat(received),payload);assert.equal(trailers['grpc-status'],'0');
  t.after(()=>api('/rules','PUT',{rules:[]}));
  await api('/rules','PUT',{rules:[{match:{url:`https://127.0.0.1:${port}/*`},request:{python:'def onRequest(c,r): return r'},response:{python:'def onResponse(c,r): return r'}}]});
  const buffered=client.request({':method':'POST',':path':'/service/Method','content-type':'application/grpc','te':'trailers'});let bufferedTrailers;const bufferedChunks=[];buffered.on('trailers',value=>{bufferedTrailers=value;});buffered.on('data',chunk=>bufferedChunks.push(chunk));const bufferedEnd=once(buffered,'end');buffered.end(payload);await bufferedEnd;assert.deepEqual(Buffer.concat(bufferedChunks),payload);assert.equal(bufferedTrailers['grpc-status'],'0');
  client.close();await once(client,'close');
  const recorded=(await api('/state')).value.sessions.find(s=>s.url.endsWith('/service/Method'));assert.equal(recorded.protocol,'HTTP/2');assert.match(recorded.responseTrailers,/grpc-status: 0/);assert.equal(recorded.grpcMessages.length,2);assert.equal(recorded.grpcMessages[1].fields[1].value.text,'hi');
});

test('binary breakpoints expose editable base64 and validate before resuming both phases', async t=>{
  t.after(()=>api('/rules','PUT',{rules:[]}));const url=`http://127.0.0.1:${origin.address().port}/binary-echo`;
  await api('/rules','PUT',{rules:[{match:{url},request:{breakpoint:true},response:{breakpoint:true}}]});
  const original=Buffer.from([0,255,1]);const updated=Buffer.from([0,254,2]);
  const response=request({host:'127.0.0.1',port:engine.proxyPort,path:url,method:'POST'},original);
  let pending=[];for(let i=0;i<50&&!pending.length;i++)pending=(await api('/breakpoints')).value.pending;
  assert.equal(pending[0].phase,'request');assert.equal(pending[0].request.bodyBase64,original.toString('base64'));assert.equal(pending[0].request.body,undefined);
  assert.equal((await api(`/breakpoints/${pending[0].id}`,'POST',{action:'continue',request:{...pending[0].request,bodyBase64:'!!'}})).status,400);
  assert.equal((await api(`/breakpoints/${pending[0].id}`,'POST',{action:'continue',request:{...pending[0].request,bodyBase64:updated.toString('base64')}})).status,200);
  pending=[];for(let i=0;i<50&&!pending.length;i++)pending=(await api('/breakpoints')).value.pending;
  assert.equal(pending[0].phase,'response');assert.equal(pending[0].response.bodyBase64,updated.toString('base64'));
  assert.equal((await api(`/breakpoints/${pending[0].id}`,'POST',{action:'continue',response:{...pending[0].response,bodyBase64:original.toString('base64')}})).status,200);
  assert.deepEqual((await response).body,original);
});

test('the reserved certificate URL serves only the current public CA through the real proxy', async t => {
  let forwarded = 0;
  const upstream = http.createServer((incoming, response) => {
    forwarded += 1;
    response.end('ordinary forwarded response');
  });
  const port = await listen(upstream);
  t.after(async () => {
    await api('/settings', 'PATCH', { upstreamEnabled: false, accessEnabled: false });
    await new Promise(resolve => upstream.close(resolve));
  });
  await api('/settings', 'PATCH', { upstreamEnabled: true, upstreamProfiles: [{ id: 'ca-upstream', name: 'CA forwarding guard', host: '127.0.0.1', port, mode: 'allow', patterns: ['*'] }], activeUpstreamProfile: 'ca-upstream' });
  const history = (await api('/history?limit=200')).value;
  const bodyFiles = await readdir(path.join(directory, 'bodies'));
  const expected = new X509Certificate(await readFile(engine.caPath));
  const download = (url = 'http://cert.recho.local/ca', method = 'GET') => request({ host: '127.0.0.1', port: engine.proxyPort, path: url, method, signal: AbortSignal.timeout(3000) });
  const certificate = await download();
  assert.equal(certificate.status, 200);
  assert.equal(certificate.headers['content-type'], 'application/x-x509-ca-cert');
  assert.equal(certificate.headers['content-disposition'], 'attachment; filename="recho-ca.crt"');
  assert.equal(Number(certificate.headers['content-length']), expected.raw.length);
  assert.deepEqual(certificate.body, expected.raw);
  const parsed = new X509Certificate(certificate.body);
  assert.equal(parsed.ca, true);
  assert.equal(parsed.verify(parsed.publicKey), true);
  assert.equal(parsed.fingerprint256, expected.fingerprint256);
  assert.throws(() => createPrivateKey({ key: certificate.body, format: 'der', type: 'pkcs8' }));
  assert.throws(() => createPrivateKey({ key: certificate.body, format: 'der', type: 'pkcs1' }));
  assert.doesNotMatch(certificate.body.toString(), /PRIVATE KEY/);
  const head = await download('http://cert.recho.local:80/ca', 'HEAD');
  assert.equal(head.status, 200);
  assert.equal(head.body.length, 0);
  assert.equal(Number(head.headers['content-length']), expected.raw.length);
  for (const url of [
    'http://cert.recho.local/ca.key',
    'http://cert.recho.local/certificates/ca.key',
    'http://cert.recho.local/%2e%2e/ca.key',
    'http://cert.recho.local/ca/../ca',
    'http://cert.recho.local/ca?file=ca.key',
    'http://cert.recho.local/CA',
    'http://cert.recho.local:81/ca',
    'https://cert.recho.local/ca',
  ]) {
    const rejected = await download(url);
    assert.equal(rejected.status, 404, url);
    assert.notDeepEqual(rejected.body, certificate.body);
    assert.doesNotMatch(rejected.body.toString(), /PRIVATE KEY/);
  }
  const rejectedMethod = await download(undefined, 'POST');
  assert.equal(rejectedMethod.status, 405);
  assert.equal(rejectedMethod.headers.allow, 'GET, HEAD');
  assert.equal(forwarded, 0);
  assert.deepEqual((await api('/history?limit=200')).value, history);
  assert.deepEqual(await readdir(path.join(directory, 'bodies')), bodyFiles);
  assert.equal((await request({ host: '127.0.0.1', port: engine.controlPort, path: '/ca' })).status, 401);
  assert.equal((await download('http://cert.recho.local.evil/ca')).body.toString(), 'ordinary forwarded response');
  assert.equal(forwarded, 1);
  await api('/settings', 'PATCH', { accessProfiles: [{ id: 'ca-deny', name: 'Deny CA download', mode: 'deny', patterns: ['127.0.0.1'] }], activeAccessProfile: 'ca-deny', accessEnabled: true });
  await assert.rejects(download(), error => ['ECONNRESET', 'EPIPE'].includes(error.code));
  assert.equal(forwarded, 1);
  assert.equal((await api('/health')).status, 200);
});


test('filter metadata records the client and interceptor kinds and survives restart', async t => {
  t.after(async () => {
    await api('/rules', 'PUT', { rules: [] });
    await api('/settings', 'PATCH', { mirrorEnabled: false, gatewayEnabled: false });
  });
  const url = 'http://filter-metadata.test/headers';
  assert.equal((await api('/settings', 'PATCH', {
    mirrorEnabled: true,
    mirrors: [{ id: 'filter', name: 'filter', host: 'filter-metadata.test', target: `127.0.0.1:${origin.address().port}`, hostPolicy: 'target' }],
    gatewayEnabled: true,
    gateways: [{ id: 'filter', name: 'filter', layer: 'l7', mode: 'allow', patterns: [url] }],
  })).status, 200);
  assert.equal((await api('/rules', 'PUT', { rules: [
    { id: 'rewrite', match: { url }, request: { headers: { 'x-filter': 'changed' } } },
    { id: 'script', match: { url },
      request: { python: "def onRequest(context, request):\n    context.shared={'saved': True}\n    request.headers['x-script']='ran'\n    print('filter-request-console')\n    return request" },
      response: { python: "def onResponse(context, response):\n    assert context.shared['saved']\n    print('filter-response-console')\n    return response" },
    },
  ] })).status, 200);
  const result = await request({ host: '127.0.0.1', port: engine.proxyPort, path: url });
  assert.equal(result.status, 200);
  assert.equal(JSON.parse(result.body)['x-filter'], 'changed');
  assert.equal(JSON.parse(result.body)['x-script'], 'ran');
  const { liveId, ...session } = (await api('/state')).value.sessions.find(item => item.url === url);
  assert.ok(Number.isInteger(liveId) && liveId > 0);
  assert.equal(session.clientIP, '127.0.0.1');
  assert.deepEqual(new Set(session.interceptors), new Set(['镜像', '网关', '重写', '脚本']));
  assert.equal(session.serverIP, '127.0.0.1');
  assert.deepEqual(session.scriptLogs, ['filter-request-console\n', 'filter-response-console\n']);
  await engine.close();
  engine = await startEngine({ token, dataDir: directory, proxyPort: 0, maxSessions: 10, upstreamCA: ca });
  assert.deepEqual((await api(`/history/${session.id}`)).value, session);
});
