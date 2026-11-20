// Loopback-only fixture for native editor verification. No system proxy or trust changes.
import http from 'node:http';
import https from 'node:https';
import http2 from 'node:http2';
import { once } from 'node:events';

export const echoProto = 'syntax = "proto3"; package local; message Message { string text = 1; } service Echo { rpc Call(Message) returns (Message); rpc Watch(Message) returns (stream Message); rpc Upload(stream Message) returns (Message); rpc Chat(stream Message) returns (stream Message); }';

export async function startProtocolFixture({ tls } = {}) {
  const activeStreams = new Set();
  const httpHandler = async (req, res) => {
    if (req.url === '/_grpc_fixture') {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ activeGrpcStreams: activeStreams.size }));
      return;
    }
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const body = JSON.stringify({ method: req.method, path: req.url, headers: req.rawHeaders, body: Buffer.concat(chunks).toString() });
    res.writeHead(200, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body), 'x-local-fixture': 'raw-http' });
    res.end(body);
  };
  const httpServer = tls ? https.createServer(tls, httpHandler) : http.createServer(httpHandler);
  const grpcServer = tls ? http2.createSecureServer(tls) : http2.createServer();
  const sessions = new Set();
  grpcServer.on('session', session => { sessions.add(session); session.once('close', () => sessions.delete(session)); });
  grpcServer.on('sessionError', () => {});
  grpcServer.on('stream', (stream, headers) => {
    activeStreams.add(stream);
    stream.once('close', () => activeStreams.delete(stream));
    stream.on('error', () => {});
    stream.respond({ ':status': 200, 'content-type': 'application/grpc', 'x-local-fixture': 'grpc' }, { waitForTrailers: true });
    stream.on('wantTrailers', () => stream.sendTrailers({ 'grpc-status': '0', 'x-local-trailer': 'done' }));
    let data = Buffer.alloc(0), last;
    stream.on('data', chunk => {
      data = Buffer.concat([data, chunk]);
      while (data.length >= 5 && data.length >= 5 + data.readUInt32BE(1)) {
        const size = 5 + data.readUInt32BE(1), message = data.subarray(0, size);
        data = data.subarray(size); last = message;
        if (headers[':path'] !== '/local.Echo/Upload') stream.write(message);
      }
    });
    stream.on('end', () => {
      if (headers[':path'] === '/local.Echo/Upload' && last) stream.write(last);
      stream.end();
    });
  });
  httpServer.listen(0, '127.0.0.1'); grpcServer.listen(0, '127.0.0.1');
  await Promise.all([once(httpServer, 'listening'), once(grpcServer, 'listening')]);
  return {
    httpPort: httpServer.address().port, grpcPort: grpcServer.address().port,
    close: () => { for (const session of sessions) session.destroy(); return Promise.all([new Promise(resolve => httpServer.close(resolve)), new Promise(resolve => grpcServer.close(resolve))]); },
  };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const fixture = await startProtocolFixture();
  process.stdout.write(JSON.stringify({ httpPort: fixture.httpPort, grpcPort: fixture.grpcPort }) + '\n');
  process.once('SIGTERM', async () => { await fixture.close(); process.exit(0); });
}
