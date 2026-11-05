"""Exercise the generator against a real bounded HTTP quota fixture."""
import importlib.util
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import http.client
import os
from pathlib import Path
import threading
import time
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('loadgen', Path(__file__).parents[1]/'loadgen.py')
loadgen = importlib.util.module_from_spec(spec)
spec.loader.exec_module(loadgen)

class GeneratorTest(unittest.TestCase):
    def test_zero_warmup_is_supported(self):
        with patch.dict(os.environ, LIMIT='2'):
            result=loadgen.phase(0,100,32,1,'warmup')
        self.assertEqual(result['offered_rps'],0)
        self.assertEqual(result['http_200'],0)

    def test_real_http_counts_match_burst_oracle(self):
        counts = {}
        lock = threading.Lock()
        class Handler(BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def log_message(self, *_): pass
            def do_GET(self):
                key = self.headers['x-bench-client']
                if key.startswith('slow'):
                    time.sleep(.2)
                with lock:
                    count = counts.get(key, 0)
                    allowed = count < 2
                    counts[key] = count + int(allowed)
                self.send_response(200 if allowed else 429)
                self.send_header('Content-Length','2')
                self.end_headers()
                self.wfile.write(b'ok')
        server = ThreadingHTTPServer(('127.0.0.1',0),Handler)
        thread = threading.Thread(target=server.serve_forever,daemon=True)
        thread.start()
        connection = http.client.HTTPConnection
        try:
            with patch.object(loadgen.http.client, 'HTTPConnection', side_effect=lambda *_a, **_k: connection('127.0.0.1',server.server_port,timeout=3)), patch.dict(os.environ, LIMIT='2'):
                result = loadgen.phase(.2,100,32,1,'test')
                slow = loadgen.phase(.05,20,32,1,'slow')
                self.assertEqual(slow['completed_in_window'],0)
                self.assertGreater(slow['drain_seconds'],.1)
                self.assertLess(slow['achieved_rps'],20)
            self.assertEqual(result['http_200'],2)
            self.assertEqual(result['expected_admissions'],2)
            self.assertEqual(result['errors'],0)
            self.assertGreater(result['http_429'],0)
            self.assertGreaterEqual(result['elapsed_seconds'],.2)
            self.assertLessEqual(result['achieved_rps'],100)
            self.assertEqual(result['sample_count']+result['dropped'],result['scheduled'])
            self.assertGreaterEqual(result['peak_in_flight'],1)
            self.assertLessEqual(result['peak_in_flight'],32)
            self.assertLessEqual(result['mean_in_flight'],result['peak_in_flight'])
            self.assertGreaterEqual(result['scheduled_latency']['p99_ms'],result['p99_ms'])
            self.assertEqual(set(result['status_latency']),{'200','429'})
        finally:
            server.shutdown(); server.server_close(); thread.join()
