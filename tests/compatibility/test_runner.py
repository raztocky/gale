import base64
import hashlib
import unittest
import json, http.client, tempfile, sys
from unittest.mock import patch
from pathlib import Path
import runner

class Checks(unittest.TestCase):

    def test_capture_scope_cannot_expand_via_redirect_or_credentials(self):
        origins = {'https://news.ycombinator.com'}
        self.assertTrue(runner.allowed_capture('https://news.ycombinator.com/news.css', origins))
        for url in ['http://news.ycombinator.com/', 'https://evil.example/', 'https://news.ycombinator.com.evil.example/', 'https://user:password@news.ycombinator.com/', 'file:///etc/passwd', 'https://127.0.0.1/', 'https://news.ycombinator.com:444/']:
            self.assertFalse(runner.allowed_capture(url, origins), url)

    def test_success_exit_and_png_do_not_make_error_page_a_pass(self):
        case = {'title_contains': ['Forum'], 'text_contains': ['Readable story'], 'min_text_chars': 10}
        good = {'title': 'Forum', 'text': 'Readable story and discussion', 'scripts': False, 'links': 3, 'requests': [], 'failed': False, 'document_status': 200}
        runner.validate_result(good, case)
        for bad in [dict(good, failed=True), dict(good, title='Error'), dict(good, text=''), dict(good, text="Couldn't load this page"), dict(good, scripts=True), dict(good, document_status=404)]:
            with self.assertRaises(ValueError):
                runner.validate_result(bad, case)

    def test_capture_hashes_and_size_are_checked_before_serving(self):
        body = b'<title>Fixture</title>'
        resource = {'url': 'https://fixture.example/', 'status': 200, 'headers': [['content-type', 'text/html']], 'body': base64.b64encode(body).decode(), 'sha256': hashlib.sha256(body).hexdigest()}
        snapshot = {'version': 1, 'name': 'test', 'entry': resource['url'], 'resources': [resource]}
        runner.validate_snapshot(snapshot)
        for name in ['../outside', '/tmp/outside', 'x/y']:
            with self.assertRaises(ValueError):
                runner.validate_snapshot(dict(snapshot, name=name))
        for changed in [dict(resource, sha256='wrong'), dict(resource, body='!!!!'), dict(resource, url='file:///tmp/x'), dict(resource, headers=[['x', 'bad\r\nheader']])]:
            with self.assertRaises(ValueError):
                runner.validate_snapshot(dict(snapshot, resources=[changed]))

    def test_replay_never_forwards_unknown_hosts(self):
        case = json.loads((Path(__file__).parent / 'fixtures/links.json').read_text())
        with tempfile.TemporaryDirectory() as t:
            replay = runner.Replay(case, Path(t))
            try:
                conn = http.client.HTTPConnection('127.0.0.1', replay.server.server_port, timeout=2)
                try:
                    conn.request('CONNECT', 'unrecorded.example:443')
                    self.assertEqual(conn.getresponse().status, 403)
                finally:
                    conn.close()
                self.assertEqual(replay.seen, [])
                self.assertTrue(replay.denied)
            finally:
                replay.close()

    def test_unavailable_capture_does_not_make_batch_successful(self):
        case = json.loads((Path(__file__).parent / 'fixtures/links.json').read_text())
        case['name'] = 'sa'

        def capture(name, path):
            if name == 'hn':
                raise ValueError('unavailable')
            return case
        result = {'wall_seconds': 0, 'replay_requests': [], 'unreplayed': []}
        with tempfile.TemporaryDirectory() as t, patch.object(sys, 'argv', ['runner', '--live', '--site', 'hn', '--site', 'sa', '--output', t]), patch.object(runner, 'capture', side_effect=capture), patch.object(runner, 'replay_case', return_value=result):
            self.assertEqual(runner.main(), 2)
if __name__ == '__main__':
    unittest.main()
