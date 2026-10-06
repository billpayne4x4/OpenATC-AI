import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

build = Path(sys.argv[1]).resolve()
name = 'open-atc-engine.exe' if os.name == 'nt' else 'open-atc-engine'
executable = build / 'Release' / name if (build / 'Release' / name).exists() else build / name
with socket.socket() as listener:
    listener.bind(('127.0.0.1', 0))
    port = listener.getsockname()[1]


def call(path, data=None, expected_status=200):
    request = Request(f'http://127.0.0.1:{port}{path}', data=None if data is None else json.dumps(data).encode(), headers={'Content-Type': 'application/json'})
    try:
        with urlopen(request, timeout=5) as response:
            assert response.status == expected_status
            return json.load(response)
    except HTTPError as error:
        assert error.code == expected_status, error.read().decode()
        return json.load(error)


with tempfile.TemporaryDirectory() as temporary:
    environment = dict(os.environ, OPENATC_CONFIG_DIR=temporary)
    process = subprocess.Popen([str(executable), str(port)], cwd=temporary, env=environment)
    try:
        for attempt in range(50):
            try:
                assert call('/health')['protocol'] == 2
                break
            except OSError:
                if process.poll() is not None:
                    raise RuntimeError('Engine exited during startup')
                time.sleep(0.1)
        else:
            raise RuntimeError('Engine did not start')
        settings = call('/state')['settings']
        settings['copilotReplies'] = True
        voice = call('/voice-health')
        assert 'stt' in voice and 'tts' in voice
        settings['copilotTunes'] = True
        settings['masterVolume'] = 0.4
        call('/settings', settings)
        assert json.loads((Path(temporary) / 'settings.json').read_text())['copilotReplies']
        assert not call('/request', {'intent': 'altitude', 'altitudeFeet': 32000})['result']['accepted']
        response = call('/request', {'intent': 'clearance'})
        assert response['result']['accepted']
        assert response['state']['clearance']['acknowledged']
        assert any(entry['speaker'] == 'COPILOT' for entry in response['state']['transcript'])
        copilot = call('/request', {'intent': 'conversation', 'role': 'copilot', 'text': 'how are you?'})
        assert not copilot['result']['accepted']
        assert 'Copilot chat needs AI' in copilot['result']['message']
        assert any(entry['speaker'] == 'COPILOT' for entry in copilot['state']['transcript'])
        assert call('/request', {'intent': 'pushback'})['state']['phase'] == 8
        response = call('/request', {'intent': 'taxi'})
        assert response['result']['accepted']
        assert response['state']['taxiClearance']['approved']
        assert len(response['state']['taxiClearance']['points']) == 3
        call('/session/save', {})
        call('/session/reset', {})
        assert call('/state')['clearance'] is None
        restored = call('/session/load', {})
        assert restored['clearance']['acknowledged']
        assert not restored['taxiClearance']['approved']
        call('/session/reset', {})
        call('/request', {'intent': 'clearance'})
        telemetry = {'altitudeFeet': 32000, 'heightAglFeet': 30000, 'groundSpeedKnots': 440, 'onGround': False, 'positionValid': True}
        for index in range(8):
            call('/telemetry', telemetry)
        assert call('/state')['phase'] == 4
        assert call('/request', {'intent': 'altitude', 'altitudeFeet': 34000})['result']['accepted']
        relay = call('/request', {'intent': 'altitude', 'altitudeFeet': 36000, 'role': 'copilot', 'text': 'Request altitude 36000 feet'})
        assert not relay['result']['accepted']
        assert 'Copilot chat needs AI' in relay['result']['message']
        assert not any(entry['speaker'] == 'COPILOT' and '36000' in entry['text'] for entry in relay['state']['transcript'])
        assert not call('/request', {'intent': 'pushback'})['result']['accepted']
        call('/session/load', {}, expected_status=400)
        call('/simbrief', {'userid': 'bad&userid=1'}, expected_status=400)
        call('/weather', {'stations': 'YMLT&format=xml'}, expected_status=400)
        settings['masterVolume'] = -1
        call('/settings', settings, expected_status=400)
        print('Engine stages, taxi approval, copilot, settings persistence and input validation passed')
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
