import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { startEngine, httpFetch as fetch } from './test-support.mjs';
import { createCertificateAuthority } from './test-certificates.mjs';
import { startProtocolFixture, echoProto } from '../scripts/protocol-echo-fixture.mjs';

test('independent raw HTTP and gRPC clients cross the authenticated control API', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'recho-protocol-api-'));
  const token = 'local-protocol-fixture';
  const fixture = await startProtocolFixture();
  const engine = await startEngine({ token, dataDir: root });
  try {
    const base = `http://127.0.0.1:${engine.controlPort}`;
    const call = async (route, input) => {
      const response = await fetch(base + route, { method: input ? 'POST' : 'GET', headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' }, body: input ? JSON.stringify(input) : undefined });
      const object = await response.json();
      assert.equal(response.status, 200, JSON.stringify(object));
      return object;
    };
    assert.equal((await fetch(base + '/grpc-client/schema', { method: 'POST', body: '{}' })).status, 401);
    const raw = await call('/request/raw', { requestID: 'integration-raw', url: `http://127.0.0.1:${fixture.httpPort}`, raw: 'PURGE /a%2fb?x=%26 HTTP/1.1\r\nHost: local\r\nX-Duplicate: one\r\nX-Duplicate: two\r\nContent-Length: 5\r\n\r\nhello', timeout: 3 });
    assert.equal(raw.status, 200);
    const echo = JSON.parse(raw.responseBody);
    assert.equal(echo.method, 'PURGE'); assert.equal(echo.path, '/a%2fb?x=%26'); assert.equal(echo.body, 'hello');
    assert.equal(echo.headers.filter(x => x === 'X-Duplicate').length, 2);
    assert.ok(Buffer.from(raw.rawResponseBase64, 'base64').toString().startsWith('HTTP/1.1 200'));
    const schemaInput = { root: 'echo.proto', sources: [{ name: 'echo.proto', source: echoProto }, { name: 'unfinished.proto', source: 'unfinished unrelated' }] };
    const schema = await call('/grpc-client/schema', schemaInput);
    assert.equal(schema.services[0].name, 'local.Echo');
    assert.equal(schema.services[0].methods.length, 4);
    const running = await call('/grpc-client/start', { descriptorSet: schema.descriptorSet, method: '/local.Echo/Call', url: `http://127.0.0.1:${fixture.grpcPort}`, messages: [{ text: 'real local call 中文' }], finish: true, timeout: 3 });
    let events;
    for (let i = 0; i < 100; i++) {
      events = await call(`/grpc-client/${running.id}/events?after=0`);
      if (events.completed) break;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.equal(events.completed, true); assert.equal(events.code, 0);
    assert.ok(events.events.some(e => e.kind === 'received' && e.value.text === 'real local call 中文'));
    assert.ok(events.events.some(e => e.kind === 'trailers'));
    await call(`/grpc-client/${running.id}/cancel`, {});
  } finally {
    await engine.close(); await fixture.close(); await rm(root, { recursive: true, force: true });
  }
});

test('independent gRPC client rejects untrusted TLS and permits an explicit per-request override', async t => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'recho-protocol-tls-'));
  let fixture, engine;
  t.after(async () => {
    try { await engine?.close(); }
    finally { try { await fixture?.close(); } finally { await rm(root, { recursive: true, force: true }); } }
  });
  const authority = await createCertificateAuthority(root);
  fixture = await startProtocolFixture({ tls: await authority.certificateForHost('127.0.0.1') });
  const token = 'local-protocol-tls';
  engine = await startEngine({ token, dataDir: root });
  const call = async (route, input) => {
    const response = await fetch(`http://127.0.0.1:${engine.controlPort}` + route, { method: input ? 'POST' : 'GET', headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' }, body: input ? JSON.stringify(input) : undefined });
    const object = await response.json();
    assert.equal(response.status, 200, JSON.stringify(object));
    return object;
  };
  const schema = await call('/grpc-client/schema', { root: 'echo.proto', sources: [{ name: 'echo.proto', source: echoProto }] });
  const invoke = async verifyTLS => {
    const started = await call('/grpc-client/start', { descriptorSet: schema.descriptorSet, method: '/local.Echo/Call', url: `https://127.0.0.1:${fixture.grpcPort}`, messages: [{ text: 'TLS + h2 中文' }], finish: true, timeout: 3, verifyTLS });
    for (let i = 0; i < 100; i++) {
      const events = await call(`/grpc-client/${started.id}/events?after=0`);
      if (events.completed) return events;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.fail('TLS gRPC call did not finish');
  };
  const rejected = await invoke(true);
  assert.notEqual(rejected.code, 0);
  assert.ok(!rejected.events.some(e => e.kind === 'received'));
  const accepted = await invoke(false);
  assert.equal(accepted.code, 0, JSON.stringify(accepted));
  assert.ok(accepted.events.some(e => e.kind === 'received' && e.value.text === 'TLS + h2 中文'));
  assert.ok(accepted.events.some(e => e.kind === 'trailers'));
});
