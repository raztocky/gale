#!/usr/bin/env python3
"""Explicit public-page captures; network-free replay with original URLs and TLS."""
from __future__ import annotations
import argparse, base64, contextlib, hashlib, http.server, json, os
from pathlib import Path
import re, signal, ssl, subprocess, tempfile, threading, time
import urllib.error, urllib.parse, urllib.request
from html.parser import HTMLParser
MAX_BODY = 2 * 1024 * 1024
MAX_TOTAL = 12 * 1024 * 1024
MAX_REQUESTS = 64
MAX_RESOURCES = 24
MAX_OUTPUT = 4 * 1024 * 1024
PRESETS = {'hn': {'name': 'hn', 'entry': 'https://news.ycombinator.com/', 'origins': ['https://news.ycombinator.com'], 'title_contains': ['Hacker News'], 'text_contains': ['Hacker News'], 'min_text_chars': 500}, 'sa': {'name': 'sa', 'entry': 'https://www.somethingawful.com/', 'origins': ['https://www.somethingawful.com', 'https://forums.somethingawful.com', 'https://fi.somethingawful.com', 'https://i.somethingawful.com'], 'title_contains': ['Something Awful'], 'text_contains': ['Forums'], 'min_text_chars': 500}, 'guardian': {'name': 'guardian', 'entry': 'https://www.theguardian.com/international', 'origins': ['https://www.theguardian.com', 'https://assets.guim.co.uk', 'https://i.guim.co.uk'], 'title_contains': ['Guardian'], 'text_contains': ['News'], 'min_text_chars': 500}}
KEEP_HEADERS = {'content-type', 'location', 'content-security-policy', 'access-control-allow-origin', 'access-control-allow-credentials', 'x-content-type-options', 'referrer-policy'}

def canonical(url):
    if not isinstance(url, str) or any((ord(c) < 32 for c in url)):
        raise ValueError('invalid URL')
    u = urllib.parse.urlsplit(url)
    if u.scheme not in ('https', 'http') or not u.hostname or u.username is not None or (u.password is not None):
        raise ValueError('invalid web URL')
    host = u.hostname.lower().encode('idna').decode()
    port = u.port
    authority = host if port in (None, 443 if u.scheme == 'https' else 80) else f'{host}:{port}'
    path = urllib.parse.quote(u.path or '/', safe="/%:@!$&'()*+,;=-._~")
    query = urllib.parse.quote(u.query, safe="/%?:@!$&'()*+,;=-._~")
    return urllib.parse.urlunsplit((u.scheme, authority, path, query, ''))

def origin(url):
    u = urllib.parse.urlsplit(canonical(url))
    return f'{u.scheme}://{u.netloc}'

def allowed_capture(url, origins):
    try:
        return canonical(url).startswith('https://') and origin(url) in origins
    except (ValueError, UnicodeError):
        return False

def validate_result(report, case):
    if report.get('document_status') != 200:
        raise ValueError('document did not load successfully')
    if report.get('failed') is not False or report.get('scripts') is not False:
        raise ValueError('error/interstitial page or scripts enabled')
    text = ' '.join(report.get('text', '').split())
    title = ' '.join(report.get('title', '').split())
    if "couldn't load this page" in text.lower() or len(text) < case.get('min_text_chars', 40):
        raise ValueError('missing readable page content')
    for key, value in (('title_contains', title), ('text_contains', text)):
        for marker in case.get(key, []):
            if marker.lower() not in value.lower():
                raise ValueError(f'missing {key} marker: {marker}')
    if report.get('links', 0) < 1:
        raise ValueError('no usable navigation links')
    if len(report.get('requests', [])) > MAX_REQUESTS:
        raise ValueError('browser exceeded request budget')

def validate_snapshot(snapshot):
    if not re.fullmatch('[a-zA-Z0-9_-]{1,64}', snapshot.get('name', '')):
        raise ValueError('invalid capture name')
    if snapshot.get('version') != 1:
        raise ValueError('unknown capture version')
    entry = canonical(snapshot['entry'])
    resources = snapshot.get('resources', [])
    if not 1 <= len(resources) <= MAX_RESOURCES:
        raise ValueError('resource count exceeds budget')
    total = 0
    seen = set()
    for r in resources:
        url = canonical(r['url'])
        if url != r['url'] or url in seen:
            raise ValueError('duplicate/noncanonical resource URL')
        seen.add(url)
        body = base64.b64decode(r['body'], validate=True)
        total += len(body)
        if len(body) > MAX_BODY or total > MAX_TOTAL:
            raise ValueError('capture body budget exceeded')
        if hashlib.sha256(body).hexdigest() != r['sha256']:
            raise ValueError('capture hash mismatch')
        if not 200 <= r['status'] <= 399:
            raise ValueError('unsupported capture status')
        if len(r['headers']) > 64:
            raise ValueError('too many headers')
        for name, value in r['headers']:
            if not re.fullmatch('[A-Za-z0-9-]+', name) or any((ord(c) < 32 for c in value)) or len(value) > 32768:
                raise ValueError('invalid capture header')
    if entry not in seen:
        raise ValueError('entry response missing')

class Assets(HTMLParser):

    def __init__(self):
        super().__init__()
        self.urls = []

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if tag == 'img' and a.get('src'):
            self.urls.append(a['src'])
        if tag == 'link' and 'stylesheet' in (a.get('rel') or '').lower().split() and a.get('href'):
            self.urls.append(a['href'])

class NoRedirect(urllib.request.HTTPRedirectHandler):

    def redirect_request(self, *args):
        return None

def capture(preset, destination):
    case = dict(PRESETS[preset])
    case['resources'] = []
    case['capture_notes'] = []
    case['version'] = 1
    queue = [case['entry']]
    seen = set()
    total = 0
    start = time.monotonic()
    opener = urllib.request.build_opener(NoRedirect())
    while queue and len(seen) < MAX_REQUESTS and (len(case['resources']) < MAX_RESOURCES) and (time.monotonic() - start < 60):
        raw = queue.pop(0)
        if not allowed_capture(raw, set(case['origins'])):
            continue
        url = canonical(raw)
        if url in seen:
            continue
        seen.add(url)
        try:
            req = urllib.request.Request(url, headers={'User-Agent': 'Brickle compatibility capture (manual)', 'Accept-Encoding': 'identity'})
            try:
                response = opener.open(req, timeout=10)
            except urllib.error.HTTPError as e:
                if 300 <= e.code < 400:
                    response = e
                else:
                    raise
            with response as response:
                status = response.code
                headers = [(k.lower(), v) for k, v in response.headers.items() if k.lower() in KEEP_HEADERS]
                kind = response.headers.get_content_type()
                if status < 300 and kind not in ('text/html', 'text/css', 'image/png', 'image/jpeg', 'image/gif', 'image/webp', 'image/svg+xml'):
                    raise ValueError('unsupported content type')
                if response.headers.get('Content-Encoding', 'identity') not in ('identity', ''):
                    raise ValueError('unexpected content encoding')
                chunks = []
                length = 0
                while True:
                    if time.monotonic() - start > 60:
                        raise ValueError('capture time budget')
                    chunk = response.read1(min(16384, MAX_BODY + 1 - length))
                    if not chunk:
                        break
                    chunks.append(chunk)
                    length += len(chunk)
                    if length > MAX_BODY:
                        raise ValueError('capture response budget')
                body = b''.join(chunks)
                if len(body) > MAX_BODY or total + len(body) > MAX_TOTAL:
                    raise ValueError('capture byte budget')
            total += len(body)
            case['resources'].append({'url': url, 'status': status, 'headers': headers, 'body': base64.b64encode(body).decode(), 'sha256': hashlib.sha256(body).hexdigest()})
            if status >= 300:
                location = next((v for k, v in headers if k == 'location'), '')
                if location:
                    queue.insert(0, urllib.parse.urljoin(url, location))
            elif kind == 'text/html':
                parser = Assets()
                parser.feed(body.decode('utf-8', errors='replace'))
                queue.extend((urllib.parse.urljoin(url, u) for u in parser.urls[:64]))
            elif kind == 'text/css':
                queue.extend((urllib.parse.urljoin(url, m.strip(' "\'')) for m in re.findall('url\\(([^)]{1,2048})\\)', body.decode('utf-8', errors='replace'))[:64]))
        except (OSError, ValueError, urllib.error.URLError) as e:
            case['capture_notes'].append({'url': url, 'error': type(e).__name__, 'reason': str(e) if isinstance(e, ValueError) else getattr(e, 'code', None)})
    case['captured_at'] = int(time.time())
    case['capture_notes'].append({'unprocessed_queue': len(queue)})
    try:
        validate_snapshot(case)
    except ValueError:
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.with_suffix('.failed.json').write_text(json.dumps(case, indent=2) + '\n')
        raise
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(case, indent=2) + '\n')
    return case

class Server(http.server.ThreadingHTTPServer):
    daemon_threads = True
    block_on_close = False

class Replay:
    """CONNECT terminates locally. This class never opens an upstream connection."""

    def __init__(self, case, work):
        validate_snapshot(case)
        self.resources = {r['url']: r for r in case['resources']}
        self.seen = []
        self.denied = []
        self.lock = threading.Lock()
        self.connections = 0
        hosts = sorted({urllib.parse.urlsplit(u).hostname for u in self.resources})
        ca = work / 'ca.pem'
        key = work / 'ca.key'
        csr = work / 'leaf.csr'
        leaf = work / 'leaf.pem'
        leafkey = work / 'leaf.key'
        ext = work / 'leaf.ext'
        ext.write_text('basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=' + ','.join(('DNS:' + h for h in hosts)) + '\n')
        commands = [['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-subj', '/CN=Brickle Offline Test CA', '-addext', 'basicConstraints=critical,CA:TRUE', '-keyout', str(key), '-out', str(ca)], ['openssl', 'req', '-new', '-newkey', 'rsa:2048', '-nodes', '-subj', '/CN=Brickle Replay', '-keyout', str(leafkey), '-out', str(csr)], ['openssl', 'x509', '-req', '-in', str(csr), '-CA', str(ca), '-CAkey', str(key), '-CAcreateserial', '-days', '1', '-extfile', str(ext), '-out', str(leaf)]]
        for cmd in commands:
            subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
        self.ca = ca
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(leaf, leafkey)
        replay = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'

            def log_message(self, *args):
                pass

            def setup(self):
                super().setup()
                self.connection.settimeout(5)

            def serve_resource(self, url):
                try:
                    url = canonical(url)
                except ValueError:
                    self.send_error(400)
                    return
                with replay.lock:
                    replay.seen.append({'method': self.command, 'url': url})
                    resource = replay.resources.get(url) if len(replay.seen) <= MAX_REQUESTS else None
                    if resource is None:
                        replay.denied.append(url)
                if self.command != 'GET':
                    self.send_error(405)
                    return
                if resource is None:
                    self.send_error(404, 'not captured or request budget reached')
                    return
                body = base64.b64decode(resource['body'])
                self.send_response(resource['status'])
                for k, v in resource['headers']:
                    self.send_header(k, v)
                self.send_header('Content-Length', str(len(body)))
                self.send_header('Connection', 'close')
                self.end_headers()
                with contextlib.suppress(BrokenPipeError, ConnectionResetError):
                    self.wfile.write(body)
                self.close_connection = True

            def do_GET(self):
                if not self.path.startswith('http://'):
                    self.send_error(400)
                    return
                self.serve_resource(self.path)

            def do_POST(self):
                with replay.lock:
                    replay.seen.append({'method': self.command, 'url': self.path})
                    replay.denied.append('forbidden method: ' + self.command)
                self.send_error(405)
            do_PUT = do_DELETE = do_PATCH = do_POST

            def do_CONNECT(self):
                try:
                    host, port = self.path.rsplit(':', 1)
                    target = f'https://{host}' + ('' if port == '443' else ':' + port)
                    with replay.lock:
                        replay.connections += 1
                        allowed = target in {origin(u) for u in replay.resources} and replay.connections <= MAX_REQUESTS
                        if not allowed:
                            replay.denied.append(target)
                    if not allowed:
                        self.send_error(403, 'origin not captured or budget reached')
                        return
                    self.send_response(200, 'Connection established')
                    self.end_headers()
                    self.wfile.flush()
                    outer = self

                    class TLSHandler(Handler):

                        def do_GET(self):
                            url = self.path if self.path.startswith('https://') else target + self.path
                            if origin(url) != target:
                                self.send_error(403)
                                return
                            self.serve_resource(url)

                        def do_CONNECT(self):
                            self.send_error(405)
                    with ctx.wrap_socket(self.connection, server_side=True) as tls:
                        TLSHandler(tls, self.client_address, self.server)
                    outer.close_connection = True
                except (OSError, ValueError):
                    self.close_connection = True
        self.server = Server(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={'poll_interval': 0.05}, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def environment(self, work):
        env = os.environ.copy()
        for k in ('HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy'):
            env[k] = f'http://127.0.0.1:{self.server.server_port}'
        env.update(NO_PROXY='', no_proxy='', SSL_CERT_FILE=str(self.ca), SSL_CERT_DIR=str(work / 'empty-roots'), XDG_CONFIG_HOME=str(work / 'config'), XDG_DATA_HOME=str(work / 'data'), HOME=str(work / 'home'))
        return env

def run_probe(binary, case, work, env):
    out = work / 'probe.json'
    err = work / 'stderr.log'
    png = work / 'page.png'
    start = time.monotonic()
    with out.open('wb') as stdout, err.open('wb') as stderr:
        p = subprocess.Popen(['prlimit', '--as=2147483648:4294967296', '--cpu=120', '--fsize=33554432', '--', str(binary), 'compat-probe', case['entry'], str(png)], env=env, cwd=work, stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr, start_new_session=True)
        try:
            while p.poll() is None:
                if time.monotonic() - start > 45 or out.stat().st_size > MAX_OUTPUT or err.stat().st_size > MAX_OUTPUT:
                    raise ValueError('browser time/output budget exceeded')
                time.sleep(0.02)
        finally:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(p.pid, signal.SIGKILL)
            p.wait()
    if p.returncode:
        raise ValueError(f"browser probe exited {p.returncode}: {err.read_text(errors='replace')[-500:]}")
    report = json.loads(out.read_text())
    validate_result(report, case)
    if not png.is_file() or png.stat().st_size > 32 * 1024 * 1024 or png.read_bytes()[:8] != b'\x89PNG\r\n\x1a\n':
        raise ValueError('missing or invalid rendered PNG')
    report['wall_seconds'] = round(time.monotonic() - start, 3)
    return report

def replay_case(binary, case, destination):
    validate_snapshot(case)
    destination.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='brickle-compat-') as temp:
        work = Path(temp)
        replay = Replay(case, work)
        try:
            filters = work / 'data/brickle/filters'
            filters.mkdir(parents=True)
            filters.joinpath('compat.txt').write_text(case.get('filters', ''))
            report = run_probe(binary, case, work, replay.environment(work))
            for forbidden in case.get('never_requested', []):
                if any((r['url'] == forbidden for r in replay.seen)):
                    raise ValueError('blocked fixture resource reached replay server')
            if case.get('require_filter_block') and (not any(('Blocked' in r['outcome'] for r in report['requests']))):
                raise ValueError('blocking control absent')
            if any((r['method'] != 'GET' for r in replay.seen)):
                raise ValueError('unexpected non-GET request')
            if len(replay.seen) > MAX_REQUESTS:
                raise ValueError('replay request budget exceeded')
            report.update(name=case['name'], mode='offline-replay', replay_requests=replay.seen, recorded_responses=sum(r["url"] in replay.resources for r in replay.seen), unreplayed=replay.denied)
            destination.joinpath('result.json').write_text(json.dumps(report, indent=2) + '\n')
            destination.joinpath('page.png').write_bytes(work.joinpath('page.png').read_bytes())
            return report
        except Exception:
            for name in ('probe.json', 'stderr.log', 'page.png'):
                source = work / name
                if source.is_file() and source.stat().st_size <= MAX_OUTPUT:
                    destination.joinpath(name).write_bytes(source.read_bytes())
            destination.joinpath('replay.json').write_text(json.dumps({'seen': replay.seen, 'denied': replay.denied}, indent=2))
            raise
        finally:
            replay.close()

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, default=Path('target/debug/brickle'))
    p.add_argument('--output', type=Path, default=Path('target/compat-results'))
    p.add_argument('--live', action='store_true', help='explicitly capture current public pages before local replay')
    p.add_argument('--site', action='append', choices=sorted(PRESETS))
    p.add_argument('--capture-dir', type=Path, default=Path('target/compat-captures'))
    p.add_argument('--snapshot', type=Path, action='append', help='replay a previously captured local snapshot')
    args = p.parse_args()
    binary = args.binary.resolve()
    cases = []
    unavailable = False
    summary = []
    if args.live:
        for name in args.site or PRESETS:
            try:
                print(f'{name}: capturing public page and approved assets', flush=True)
                cases.append(capture(name, args.capture_dir / (name + '.json')))
            except (OSError, ValueError) as e:
                unavailable = True
                summary.append({"name":name,"status":"capture-unavailable","error":type(e).__name__})
                print(f'{name}: capture unavailable ({type(e).__name__}); no compatibility result', flush=True)
    elif args.site:
        p.error('--site requires --live')
    elif args.snapshot:
        cases = [json.loads(f.read_text()) for f in args.snapshot]
    else:
        cases = [json.loads(f.read_text()) for f in sorted((Path(__file__).parent / 'fixtures').glob('*.json'))]
    if not cases:
        return 2
    failed = False
    for case in cases:
        try:
            result = replay_case(binary, case, args.output / case['name'])
            summary.append({'name':case['name'],'status':'pass','seconds':result['wall_seconds']})
            print(f"{case['name']}: PASS ({result['wall_seconds']}s, {len(result['replay_requests'])} requests, {len(result['unreplayed'])} unreplayed)", flush=True)
        except (OSError, ValueError, subprocess.SubprocessError) as e:
            failed = True
            summary.append({"name":case["name"],"status":"fail","error":str(e)})
            print(f"{case['name']}: FAIL {e}", flush=True)
    args.output.mkdir(parents=True,exist_ok=True)
    args.output.joinpath("summary.json").write_text(json.dumps(summary,indent=2)+"\n")
    return 1 if failed else 2 if unavailable else 0
if __name__ == '__main__':
    raise SystemExit(main())
