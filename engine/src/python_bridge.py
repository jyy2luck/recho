"""HTTP request/response hook API for Recho Python scripts.

Runs only user-supplied local scripts. It is not a Python sandbox.
"""
import contextlib
import base64
import email
import email.policy
import http.client
import json
import os
import sys
import time
import traceback
import types
import urllib.parse
import uuid

# Keep hook console output consistent across Windows and Unix hosts.
sys.stderr.reconfigure(encoding="utf-8", newline="\n")


class HttpQueries:
    def __init__(self, value=None):
        self._raw = value if isinstance(value, str) else None
        self._dirty = False
        self._pairs = urllib.parse.parse_qsl(value, keep_blank_values=True) if isinstance(value, str) else list((value or {}).items() if isinstance(value, dict) else (value or []))

    def __len__(self): return len(self._pairs)
    def __iter__(self): return iter(self._pairs)
    def __str__(self): return str(self._pairs)
    def _name(self, value): return value
    def indexes(self, name): return [i for i, (key, _) in enumerate(self._pairs) if self._name(key) == self._name(name)]
    def index(self, name): return next(iter(self.indexes(name)), -1)
    def __getitem__(self, key):
        if isinstance(key, int): return self._pairs[key]
        index = self.index(key)
        return self._pairs[index][1] if index >= 0 else None
    def __setitem__(self, key, value):
        self._dirty = True
        index = self.index(key)
        if index >= 0: self._pairs[index] = (self._pairs[index][0], str(value))
        else: self.add(key, value)
    def __delitem__(self, key): self.remove(key)
    def add(self, name, value):
        self._dirty = True
        self._pairs.append((str(name), str(value)))
    def remove(self, name):
        self._dirty = True
        self._pairs = [(k, v) for k, v in self._pairs if self._name(k) != self._name(name)]
    def clear(self):
        self._dirty = True
        self._pairs.clear()
    def concat(self, encode=False):
        return urllib.parse.urlencode(self._pairs) if encode else '&'.join(k + '=' + v for k, v in self._pairs)
    def wire(self): return self._raw if self._raw is not None and not self._dirty else self.concat(encode=True)


class HttpHeaders(HttpQueries):
    def __init__(self, value=None):
        self._raw = None
        self._dirty = False
        self._pairs = []
        if isinstance(value, dict):
            for key, values in value.items():
                for item in values if isinstance(values, list) else [values]: self.add(key, item)
        else:
            for item in (value.splitlines() if isinstance(value, str) else value or []):
                key, val = item.split(':', 1) if isinstance(item, str) else item
                self.add(key.strip(), val.strip())
    def _name(self, value): return value.lower()
    def __iter__(self): return iter([k + ': ' + v for k, v in self._pairs])
    def __getitem__(self, key):
        if isinstance(key, int): return ': '.join(self._pairs[key])
        return super().__getitem__(key)
    def get(self, key, default=None):
        value = self[key]
        return default if value is None else value
    def wire(self):
        result = {}
        for key, value in self._pairs:
            key = key.lower()
            if key not in result: result[key] = value
            elif isinstance(result[key], list): result[key].append(value)
            else: result[key] = [result[key], value]
        return result


class HttpBody:
    def __init__(self, value=None): self._value = value
    @property
    def isNone(self): return self._value is None
    @property
    def isText(self): return isinstance(self._value, (str, dict, list)) and not self.isMultipart
    @property
    def isBinary(self): return isinstance(self._value, bytes)
    @property
    def isMultipart(self): return isinstance(self._value, list) and bool(self._value) and all(isinstance(v, HttpMultipartBody) for v in self._value)
    @property
    def type(self): return 0 if self.isNone else 3 if self.isMultipart else 2 if self.isBinary else 1
    @property
    def payload(self): return self._value
    def none(self): self._value = None
    def text(self, value): self._value = str(value)
    def textFromFile(self, path):
        with open(os.path.expanduser(path), encoding='utf-8') as source: self._value = source.read()
    def binary(self, value):
        if isinstance(value, str):
            with open(os.path.expanduser(value), 'rb') as source: self._value = source.read()
        else: self._value = bytes(value)
    def file(self, path): self.binary(path)
    def multiparts(self, values):
        if not all(isinstance(v, HttpMultipartBody) for v in values): raise TypeError('multiparts requires HttpMultipartBody values')
        self._value = list(values)
    def writeFile(self, path):
        with open(os.path.expanduser(path), 'wb') as target:
            if self.isMultipart: raise TypeError('writeFile does not support multipart bodies')
            target.write(self._value if self.isBinary else str(self).encode('utf-8'))
    def jsonify(self):
        if not isinstance(self._value, (dict, list)): self._value = json.loads(str(self))
        return self._value
    def __getitem__(self, key): return self._value[key]
    def __setitem__(self, key, value): self._value[key] = value
    def __delitem__(self, key): del self._value[key]
    def __iter__(self): return iter(self._value or [])
    def __len__(self): return len(self._value or '')
    def __str__(self):
        if self._value is None: return ''
        if self.isMultipart: return str([str(v) for v in self._value])
        if isinstance(self._value, (dict, list)): return json.dumps(self._value, ensure_ascii=False, separators=(',', ':'))
        if isinstance(self._value, bytes): return self._value.decode('utf-8')
        return str(self._value)
    def wire(self, content_type=None):
        if self.isBinary: return {'bodyBase64': base64.b64encode(self._value).decode('ascii')}
        if self.isMultipart:
            message = email.message.Message()
            message['content-type'] = content_type or ''
            boundary = message.get_boundary()
            if not boundary or '\r' in boundary or '\n' in boundary: raise ValueError('Multipart Content-Type requires a valid boundary')
            chunks = []
            for part in self._value:
                chunks.append(('--' + boundary + '\r\n').encode())
                chunks.append(('\r\n'.join(part.headers) + '\r\n\r\n').encode())
                chunks.append(part.payload if part.isBinary else str(part).encode())
                chunks.append(b'\r\n')
            chunks.append(('--' + boundary + '--\r\n').encode())
            return {'bodyBase64': base64.b64encode(b''.join(chunks)).decode('ascii')}
        return {'body': str(self)}


class HttpMultipartBody(HttpBody):
    def __init__(self, value=None, name=None, filename=None, headers=None):
        super().__init__(value)
        self.headers = HttpHeaders(headers)
        self._name = name
        self._filename = filename
        if name is not None: self._disposition()
    def _disposition(self):
        def escape(value): return str(value).replace('\\', '\\\\').replace('"', '\\"').replace('\r', '').replace('\n', '')
        value = 'form-data; name="' + escape(self._name or '') + '"'
        if self._filename is not None: value += '; filename="' + escape(self._filename) + '"'
        self.headers['content-disposition'] = value
    @property
    def name(self): return self._name
    @name.setter
    def name(self, value): self._name = value; self._disposition()
    @property
    def filename(self): return self._filename
    @filename.setter
    def filename(self, value): self._filename = value; self._disposition()
    @classmethod
    def text(cls, value, name=None, filename=None): return cls(str(value), name, filename)
    @classmethod
    def file(cls, path, name=None, filename=None):
        with open(os.path.expanduser(path), 'rb') as source: value = source.read()
        return cls(value, name, filename or os.path.basename(path))


class Message:
    protocol = 'HTTP/1.1'
    def __init__(self, data):
        self.headers = data.get('headers', {})
        self.body = base64.b64decode(data['bodyBase64'], validate=True) if 'bodyBase64' in data else data.get('body')
        if self.contentType and self.contentType.lower().startswith('multipart/'):
            raw = self.body.payload if self.body.isBinary else str(self.body).encode()
            message = email.message_from_bytes(('Content-Type: '+self.contentType+'\r\nMIME-Version: 1.0\r\n\r\n').encode()+raw, policy=email.policy.default)
            if message.is_multipart():
                self.body.multiparts([HttpMultipartBody(part.get_payload(decode=True), part.get_param('name', header='content-disposition'), part.get_filename(), list(part.raw_items())) for part in message.iter_parts()])
        self.trailers = HttpHeaders()
    def __setattr__(self, name, value):
        if name == 'headers' and not isinstance(value, HttpHeaders): value = HttpHeaders(value)
        if name == 'queries' and not isinstance(value, HttpQueries): value = HttpQueries(value)
        if name == 'body' and not isinstance(value, HttpBody): value = HttpBody(value)
        object.__setattr__(self, name, value)
    @property
    def contentType(self): return self.headers['content-type']
    @property
    def mime(self): return self.contentType.split(';')[0].strip() if self.contentType else None


class HttpRequest(Message):
    def __init__(self, data):
        super().__init__(data)
        self.method = data['method']
        self.url = data['url']
        parsed = urllib.parse.urlsplit(self.url)
        self.path = parsed.path or '/'
        self.queries = HttpQueries(parsed.query)
        self._initial_path = self.path
    def wire(self):
        parsed = urllib.parse.urlsplit(self.url)
        # A script changing URL directly can leave path/query untouched.
        path = self.path if self.path != self._initial_path else parsed.path
        query = self.queries.wire()
        return {'method': self.method, 'url': urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, path, query, '')), 'headers': self.headers.wire(), **self.body.wire(self.contentType)}


class HttpResponse(Message):
    def __init__(self, data, request):
        super().__init__(data)
        self.request = request
        self.code = data['status']
    @property
    def status(self): return self.code
    @status.setter
    def status(self, value): self.code = value
    @property
    def message(self): return http.client.responses.get(self.code, '')
    def wire(self): return {'status': self.code, 'headers': self.headers.wire(), **self.body.wire(self.contentType)}


class Context:
    def __init__(self, request, envelope):
        parsed = urllib.parse.urlsplit(request.url)
        self.url, self.scheme, self.host = request.url, parsed.scheme, parsed.hostname
        self.port = parsed.port or (443 if parsed.scheme == 'https' else 80)
        metadata = envelope.get('_context', {}) if isinstance(envelope, dict) else {}
        self.uid = metadata.get('uid') or str(uuid.uuid4())
        self.timestamp = metadata.get('timestamp') or int(time.time() * 1000)
        self.id = metadata.get('id', 0)
        self.shared = envelope.get('_shared') if isinstance(envelope, dict) else envelope
        self.env = metadata.get('env', {})
        self.app = self.connection = None
        self.highlight = metadata.get('highlight', 'none')
    def wire(self):
        return {'_context': {'uid': self.uid, 'timestamp': self.timestamp, 'id': self.id, 'env': self.env, 'highlight': self.highlight}, '_shared': self.shared}


class Highlight:
    none, red, yellow, green, blue, teal, strikethrough = 'none', 'red', 'yellow', 'green', 'blue', 'teal', 'strikethrough'


def main():
    payload = json.load(sys.stdin)
    request = HttpRequest(payload['request'])
    response = HttpResponse(payload['response'], request) if payload.get('response') else None
    context = Context(request, payload.get('shared'))
    exports = {key: value for key, value in globals().copy().items() if key in ['Context', 'HttpRequest', 'HttpResponse', 'HttpHeaders', 'HttpQueries', 'HttpBody', 'HttpMultipartBody', 'Highlight']}
    exports['CaptureContext'] = Context
    module = types.ModuleType('recho')
    module.__dict__.update(exports)
    sys.modules['recho'] = module
    namespace = {'__name__': '__recho_script__', **exports}
    with contextlib.redirect_stdout(sys.stderr):
        exec(compile(payload['source'], '<recho-script>', 'exec'), namespace)
        hook = namespace.get('onResponse' if response else 'onRequest')
        value = hook(context, response if response else request) if hook else (response if response else request)
        if value is None: raise RuntimeError('Script returned None; request was interrupted')
        if response:
            if not isinstance(value, HttpResponse): raise TypeError('onResponse must return HttpResponse or None')
            response = value
        else:
            if not isinstance(value, HttpRequest): raise TypeError('onRequest must return HttpRequest or None')
            request = value
        result = {'request': request.wire(), 'shared': context.wire()}
        if response: result['response'] = response.wire()
    json.dump(result, sys.stdout, ensure_ascii=False, allow_nan=False)


if __name__ == '__main__':
    try: main()
    except BaseException:
        traceback.print_exc(file=sys.stderr)
        sys.exit(1)
