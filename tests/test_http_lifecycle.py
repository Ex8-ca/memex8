"""HTTP transport regression tests; disposable loopback Qdrant/mock LLM only."""
import http.client
import json
import socket
import subprocess
import time
import unittest
from urllib.parse import urlsplit
from test_consolidation import fixture, points


class HttpLifecycle(unittest.TestCase):
    def test_missing_key_is_fail_closed(self):
        from test_certification import wait_ready
        with fixture() as f:
            env = dict(f['env'])
            env.pop('MEMEX8_API_KEY', None)
            with socket.socket() as sock:
                sock.bind(('127.0.0.1', 0))
                port = sock.getsockname()[1]
            process = subprocess.Popen(f['command'] + ['serve', '--host', '127.0.0.1', '--port', str(port)],
                env=env, cwd=f['work'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                wait_ready(f'http://127.0.0.1:{port}/health')
                for path in ('/webhooks/conversation', '/webhooks/skill',
                             '/api/v1/webhooks/conversation', '/api/v1/webhooks/skill',
                             '/mcp', '/mcp/messages'):
                    c = http.client.HTTPConnection('127.0.0.1', port, timeout=5)
                    c.request('GET' if path == '/mcp' else 'POST', path, '{}',
                              {'Content-Type': 'application/json', 'Authorization': 'Bearer test-only'})
                    r = c.getresponse()
                    self.assertEqual(r.status, 401, path)
                    r.read()
                    c.close()
                self.assertEqual(points(f), [])
            finally:
                process.terminate()
                process.wait(timeout=10)

    def test_auth_sessions_and_cleanup(self):
        with fixture() as f:
            url = urlsplit(f['api_url'])
            connections = []

            def request(path, value=None, token: str | None = 'test-only', method='POST', raw=None):
                c = http.client.HTTPConnection(url.hostname, url.port, timeout=5)
                headers = {'Content-Type': 'application/json'}
                if token is not None:
                    headers['Authorization'] = 'Bearer ' + token
                c.request(method, path, body=raw if raw is not None else json.dumps(value), headers=headers)
                r = c.getresponse()
                status, body = r.status, r.read()
                c.close()
                return status, body

            def event(response):
                fields = {}
                while True:
                    line = response.readline().decode().strip()
                    if not line:
                        if fields:
                            return fields
                        continue
                    if ':' in line:
                        k, v = line.split(':', 1)
                        fields[k] = v.strip()

            def connect():
                c = http.client.HTTPConnection(url.hostname, url.port, timeout=5)
                c.request('GET', '/mcp', headers={'Authorization': 'Bearer test-only'})
                r = c.getresponse()
                self.assertEqual(r.status, 200)
                first = event(r)
                self.assertEqual(first['event'], 'endpoint')
                self.assertTrue(first['data'].startswith('/mcp/messages?session_id='))
                connections.append((c, r))
                return r, first['data']

            try:
                before = points(f)
                for token in (None, 'wrong'):
                    for path in ('/webhooks/conversation', '/webhooks/skill',
                                 '/api/v1/webhooks/conversation', '/api/v1/webhooks/skill',
                                 '/mcp/messages'):
                        self.assertEqual(request(path, {}, token)[0], 401, path)
                    self.assertEqual(request('/mcp', token=token, method='GET')[0], 401)
                self.assertEqual(points(f), before)
                # Authenticated aliases reach JSON validation, not a bypass or a 404.
                for path in ('/webhooks/conversation', '/webhooks/skill',
                             '/api/v1/webhooks/conversation', '/api/v1/webhooks/skill'):
                    self.assertEqual(request(path, raw='{')[0], 400)

                a, endpoint_a = connect()
                b, endpoint_b = connect()
                self.assertNotEqual(endpoint_a, endpoint_b)
                def rpc(endpoint, ident, method, params=None):
                    value = {'jsonrpc': '2.0', 'method': method}
                    if ident is not None:
                        value['id'] = ident
                    if params is not None:
                        value['params'] = params
                    self.assertEqual(request(endpoint, value)[0], 202)

                rpc(endpoint_a, 1, 'initialize', {'protocolVersion': '2024-11-05', 'capabilities': {},
                                                'clientInfo': {'name': 'test', 'version': '1'}})
                self.assertEqual(json.loads(event(a)['data'])['id'], 1)
                rpc(endpoint_a, None, 'notifications/initialized')
                rpc(endpoint_b, 'b', 'ping')
                self.assertEqual(json.loads(event(b)['data'])['id'], 'b')
                rpc(endpoint_a, 2, 'tools/call', {'name': 'memex8_remember',
                    'arguments': {'content': 'isolated HTTP lifecycle fixture'}})
                response = json.loads(event(a)['data'])
                self.assertEqual(response['id'], 2)  # also proves no notification reply/cross-session leak
                self.assertIn('result', response)
                self.assertEqual(len(points(f)), 1)
                rpc(endpoint_a, 3, 'unknown-method')
                self.assertEqual(json.loads(event(a)['data'])['error']['code'], -32601)
                for value in ([], {}, {'jsonrpc': '2.0', 'method': 'ping', 'id': {}},
                              {'jsonrpc': '2.0', 'method': 'ping', 'params': 'bad'}):
                    self.assertEqual(request(endpoint_a, value)[0], 400)
                self.assertEqual(request(endpoint_a, raw='{')[0], 400)
                self.assertEqual(request(endpoint_a, raw='x' * (1024 * 1024 + 1))[0], 413)
                for query in ('', '?session_id=bad', '?session_id=',
                              endpoint_a.split('?')[1].join(['?', '&session_id=bad'])):
                    self.assertEqual(request('/mcp/messages' + query, {})[0], 400)
                self.assertEqual(request('/mcp/messages?session_id=00000000-0000-0000-0000-000000000000', {})[0], 404)
                # Abrupt idle SSE disconnect must release entries without another message.
                for c, r in connections:
                    r.close()
                    c.close()
                connections.clear()
                deadline = time.monotonic() + 5
                while request(endpoint_a, {})[0] != 404 and time.monotonic() < deadline:
                    time.sleep(.02)
                self.assertEqual(request(endpoint_a, {})[0], 404)
                self.assertEqual(request(endpoint_b, {})[0], 404)
                # Fill the bounded registry and prove both limit and slot reclamation.
                for _ in range(64):
                    connect()
                self.assertEqual(request('/mcp', method='GET')[0], 503)
            finally:
                for c, r in connections:
                    r.close()
                    c.close()


if __name__ == '__main__':
    unittest.main(verbosity=2)
