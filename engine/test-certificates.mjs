// Legacy-format CA fixture for the Rust engine integration tests; never bundled.
import { execFile } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { chmod, mkdir, mkdtemp, readFile, rename, rm, stat, writeFile } from 'node:fs/promises';
import { isIP } from 'node:net';
import path from 'node:path';
import { promisify } from 'node:util';
import { domainToASCII } from 'node:url';

const run = promisify(execFile);

export async function createCertificateAuthority(dataDir) {
  const directory = path.join(dataDir, 'certificates');
  const caPath = path.join(directory, 'ca.pem');
  const keyPath = path.join(directory, 'ca.key');
  await mkdir(directory, { recursive: true, mode: 0o700 });
  await chmod(directory, 0o700);
  const exists = async file => stat(file).then(() => true, error => {
    if (error.code === 'ENOENT') return false;
    throw error;
  });
  const [hasCertificate, hasKey] = await Promise.all([exists(caPath), exists(keyPath)]);
  if (hasCertificate !== hasKey) throw new Error('Local CA is incomplete. Restore both ca.pem and ca.key, or remove the certificates directory to create a new CA.');
  const openssl = args => run(process.env.RECHO_TEST_OPENSSL || 'openssl', args, { timeout: 20_000, maxBuffer: 256 * 1024, windowsHide: true });
  if (!hasCertificate) {
    const temporary = await mkdtemp(path.join(directory, '.ca-'));
    try {
      const configuration = path.join(temporary, 'ca.cnf');
      await writeFile(configuration, '[req]\ndistinguished_name=dn\nx509_extensions=ca\nprompt=no\n[dn]\nCN=Recho Local CA\n[ca]\nbasicConstraints=critical,CA:true\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\n', { mode: 0o600 });
      await openssl(['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-sha256', '-days', '3650', '-config', configuration, '-keyout', path.join(temporary, 'key.pem'), '-out', path.join(temporary, 'ca.pem')]);
      await chmod(path.join(temporary, 'key.pem'), 0o600);
      await rename(path.join(temporary, 'key.pem'), keyPath);
      await rename(path.join(temporary, 'ca.pem'), caPath);
    } finally {
      await rm(temporary, { recursive: true, force: true });
    }
  }
  await chmod(keyPath, 0o600);
  // Validate the persisted pair before accepting any intercepted connections.
  const [certificatePublicKey, privatePublicKey] = await Promise.all([
    openssl(['x509', '-in', caPath, '-pubkey', '-noout']),
    openssl(['pkey', '-in', keyPath, '-pubout']),
  ]);
  if (certificatePublicKey.stdout.trim() !== privatePublicKey.stdout.trim()) throw new Error('The local CA certificate and private key do not match.');
  const cache = new Map();

  async function certificateForHost(hostname) {
    const host = isIP(hostname) ? hostname : domainToASCII(hostname).toLowerCase();
    if (!host || (!isIP(host) && (host.length > 253 || host.split('.').some(label => !/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label))))) {
      throw new Error('Invalid TLS hostname');
    }
    if (cache.has(host)) return cache.get(host);
    const pending = (async () => {
      const temporary = await mkdtemp(path.join(directory, '.leaf-'));
      try {
        const key = path.join(temporary, 'key.pem');
        const csr = path.join(temporary, 'request.pem');
        const cert = path.join(temporary, 'cert.pem');
        const extensions = path.join(temporary, 'extensions.cnf');
        await writeFile(extensions, `basicConstraints=critical,CA:false\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=${isIP(host) ? 'IP' : 'DNS'}:${host}\n`, { mode: 0o600 });
        await openssl(['req', '-new', '-newkey', 'rsa:2048', '-nodes', '-sha256', '-subj', `/CN=${host}`, '-keyout', key, '-out', csr]);
        await openssl(['x509', '-req', '-in', csr, '-CA', caPath, '-CAkey', keyPath, '-set_serial', `0x${randomBytes(16).toString('hex')}`, '-days', '30', '-sha256', '-extfile', extensions, '-out', cert]);
        return { key: await readFile(key), cert: await readFile(cert) };
      } finally {
        await rm(temporary, { recursive: true, force: true });
      }
    })();
    cache.set(host, pending);
    if (cache.size > 100) cache.delete(cache.keys().next().value);
    try {
      return await pending;
    } catch (error) {
      cache.delete(host);
      throw error;
    }
  }
  return { caPath, certificateForHost };
}
