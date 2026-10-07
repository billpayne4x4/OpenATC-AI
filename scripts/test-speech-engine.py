"""Check the real /suggest pipeline against a local fake model, with isolated settings."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('engine', type=Path)
parser.add_argument('speech', type=Path)
args = parser.parse_args()
captured = []

class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        captured.append(body['messages'][0]['content'])
        data = json.dumps({'choices': [{'message': {'content': 'Test proposal only.'}}]}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

model = ThreadingHTTPServer(('127.0.0.1', 0), Model)
thread = threading.Thread(target=model.serve_forever, daemon=True)
thread.start()
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]

def request(path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = Request(f'http://127.0.0.1:{port}/{path}', data=data, headers={'Content-Type':'application/json'})
    try:
        with urlopen(req, timeout=5) as response:
            return response.status, json.load(response)
    except HTTPError as error:
        return error.code, error.read().decode()

try:
    with tempfile.TemporaryDirectory(prefix='openatc-speech-api-') as temporary:
        env = dict(os.environ, OPENATC_CONFIG_DIR=temporary,
                   OPENATC_SPEECH_DIR=str(args.speech.resolve()),
                   OPENATC_AI_URL=f'http://127.0.0.1:{model.server_port}/v1',
                   OPENATC_AI_MODEL='local-test-model')
        with open(Path(temporary)/'engine.log','w') as log:
            engine = subprocess.Popen([str(args.engine.resolve()), str(port)],env=env,stdout=log,stderr=log)
            try:
                for attempt in range(50):
                    if engine.poll() is not None:
                        raise RuntimeError('test engine exited')
                    try:
                        if request('health')[0] == 200:
                            break
                    except (URLError, TimeoutError):
                        time.sleep(.1)
                else:
                    raise RuntimeError('test engine did not become ready')
                cases = [
                    ('EGLL','vfr','cruise','basic_service','united_kingdom','basic service'),
                    ('KJFK','ifr','departure','climb_via','us','climb via SID'),
                    ('YMLT','ifr','taxi','readback_taxi','australia','holding point'),
                ]
                for airport,rules,phase,tag,region,phrase in cases:
                    status,body = request('suggest',{'role':'atc','airport':airport,'flightRules':rules,'phase':phase,'situations':[tag],'facts':'Testing selection only; do not issue an operational clearance.'})
                    assert status == 200, (status, body)
                    assert body['transmission'] == 'Test proposal only.'
                    assert f'Regional scope: {region}.' in captured[-1]
                    assert phrase in captured[-1]
                    assert f'flight rules {rules}' in captured[-1]
                before = len(captured)
                for body in [
                    {'role':'atc','airport':'YMLT','flightRules':'vfr','phase':'cruise','situations':['basic_service']},
                    {'role':'atc','airport':'KJFK','flightRules':'ifr','phase':'departure','situations':['conditional_lineup_traffic_identified_and_in_sight']},
                    {'role':'atc','airport':'KJFK','flightRules':'invalid'},
                ]:
                    assert request('suggest',body)[0] == 400
                assert len(captured) == before, 'invalid regional/rules selection reached the model'
                print('Real engine /suggest: 3 scoped selections and 3 rejected requests passed with local fake model')
            finally:
                engine.terminate()
                engine.wait(timeout=5)
finally:
    model.shutdown()
    model.server_close()
    thread.join(timeout=5)
