"""Exercise the real Rust engine with isolated scenery, settings and a fake LLM."""
import argparse, json, math, os, socket, subprocess, shutil, tempfile, threading, time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.request import Request, urlopen
from urllib.error import HTTPError, URLError
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('engine',type=Path);parser.add_argument('speech',type=Path)
args=parser.parse_args()
captured=[]
class Model(BaseHTTPRequestHandler):
    def log_message(self,*args): pass
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        captured.append(body)
        # Deliberately corrupt operational data: engine must reject this wording.
        data=json.dumps({'choices':[{'message':{'content':'Cleared to FAKE. Climb to 99000 feet, squawk 7777.'}}]}).encode()
        self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
model=ThreadingHTTPServer(('127.0.0.1',0),Model)
threading.Thread(target=model.serve_forever,daemon=True).start()
with socket.socket() as sock:
    sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
def call(path,body=None):
    req=Request(f'http://127.0.0.1:{port}/{path}',data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
    try:
        with urlopen(req,timeout=35) as res: return res.status,json.load(res)
    except HTTPError as err: return err.code,err.read().decode()
def ok(path,body=None):
    status,result=call(path,body);assert status==200,(path,status,result);return result
with tempfile.TemporaryDirectory(prefix='openatc-radio-') as temporary:
    folder=Path(temporary);root=folder/'sim'
    speech_copy=folder/'speech';shutil.copytree(args.speech.resolve(),speech_copy)
    runtime=speech_copy/'runtime/responses.toml'
    text=runtime.read_text();begin=text.index('[readback_correct_for_recorded_altitude_and_route]');end=text.find('\n[',begin+1)
    text=text[:begin]+"[readback_correct_for_recorded_altitude_and_route]\nslots = []\nsay = ['Custom TOML readback accepted for recorded altitude and route.']\n"+text[end:]
    begin=text.index('[airport_identifier_not_found_in_apt_dat]');end=text.find('\n[',begin+1)
    text=text[:begin]+"[airport_identifier_not_found_in_apt_dat]\nslots = []\nsay = ['Custom edited missing airport wording.']\n"+text[end:]
    runtime.write_text(text)
    apt=root/'Global Scenery/Global Airports/Earth nav data/apt.dat';apt.parent.mkdir(parents=True)
    # 09 threshold at (0,0); stand west/north of the threshold, taxiway Alpha
    # approaches the threshold without an implicit runway crossing.
    runway='100 45 1 1 0.25 0 3 0 09 0 0 0 0 2 0 0 0 27 0 0.02 0 0 2 0 0 0\n'
    apt.write_text('I\n1200 Version\n1 0 0 0 TEST Test International\n'+runway+
        '1050 118000 Test ATIS\n1052 121800 Test Delivery\n1053 121900 Test Ground\n1054 118700 Test Tower\n1055 119000 Test Approach\n1056 119100 Test Departure\n'+
        '1201 0.003 -0.003 both 1\n1201 0.001 -0.001 both 2\n1201 0.001 0.000 both 3\n1201 0 0 both 4\n1202 1 2 twoway taxiway_C Alpha\n1202 2 3 twoway taxiway_C Alpha\n1202 3 4 twoway taxiway_C Alpha\n1300 0.003 -0.003 90 gate jets Stand 1\n'+
        '1 0 0 0 DEST Destination International\n'+runway.replace('09 0 0','09 0 0.1').replace('27 0 0.02','27 0 0.12')+'1050 118100 Destination ATIS\n1054 118700 Destination Tower\n'+'1 0 0 0 RWYO Runway Only\n'+runway+'1053 122000 Runway Only Ground\n1054 122100 Runway Only Tower\n110 1 0.2 0 Apron\n111 -0.001 0.007\n111 -0.001 0.009\n111 0.001 0.009\n113 0.001 0.007\n99\n')
    custom=root/'Custom Scenery/Unrelated/Earth nav data/apt.dat';custom.parent.mkdir(parents=True)
    custom.write_text('1 0 0 0 OTHER Unrelated scenery\n'+runway+'99\n')
    (root/'Custom Scenery/scenery_packs.ini').write_text('SCENERY_PACK Custom Scenery/Unrelated/\n')
    config=folder/'config';config.mkdir()
    (config/'settings.json').write_text(json.dumps({'simulatorRoot':str(root),'aiEnabled':False,'copilotReplies':False,'copilotAutoRespond':True,'congestion':'off','strictReadbacks':True}))
    env=dict(os.environ,OPENATC_CONFIG_DIR=str(config),OPENATC_SPEECH_DIR=str(speech_copy),OPENATC_AI_URL=f'http://127.0.0.1:{model.server_port}',OPENATC_AI_MODEL='fake')
    log=open(folder/'engine.log','w')
    engine=subprocess.Popen([str(args.engine.resolve()),str(port)],env=env,stdout=log,stderr=log)
    try:
        for attempt in range(100):
            try:
                if call('health')[0]==200: break
            except URLError:time.sleep(.05)
        else:raise AssertionError('engine failed to start')
        telemetry={'latitude':0.003,'longitude':-0.003,'altitudeFeet':0,'onGround':True,'positionValid':True,'radioPower':True,'com1Khz':123450,'traffic':[]}
        def tune(freq,**kwargs):
            telemetry.update(com1Khz=freq,**kwargs);ok('telemetry',telemetry)
        def req(intent,**kwargs):return ok('request',dict({'intent':intent,'text':intent,'role':'atc'},**kwargs))
        tune(123450)
        stations=ok('stations/nearby',{})['stations'];assert len(stations)==10,stations
        for intent in ['radio_check','clearance','emergency']:
            result=req(intent);assert result['result']['silent'] and not result['result']['message'];assert not result['state']['transcript']
        plan={'departure':'TEST','destination':'DEST','callsign':'VH-BIL','runway':'09','arrivalRunway':'09','initialAltitudeFeet':5000,'cruiseFeet':25000,'route':'DCT DEST'}
        ok('plan',plan)
        tune(121900);assert not req('clearance')['result']['accepted']
        tune(121800)
        wrong=dict(plan,departure='DEST',destination='TEST',route='DCT TEST')
        ok('plan',wrong)
        rejected=req('clearance');assert not rejected['result']['accepted']
        assert 'Your flight departs DEST' in rejected['result']['message'],rejected
        assert rejected['state']['transcript'][-1]['text']==rejected['result']['message'],rejected
        ok('plan',plan)
        result=req('clearance');assert result['result']['accepted'],result
        message=result['result']['message'];assert all(v in message for v in ['VH-BIL','DEST','5000','2105','09','DCT DEST']),message
        clearance=result['state']['clearance'];assert not clearance['acknowledged']
        tune(121900);assert not req('start')['result']['accepted']
        tune(121800)
        bad=ok('request', {'intent':'conversation','role':'atc','text':'Victor Hotel Bravo India Lima cleared DEST five hundred feet squawk two one zero five runway zero nine'})
        assert not bad['result']['accepted'] and 'say again' in bad['result']['message'].lower(),bad
        good=ok('request', {'intent':'conversation','role':'atc','text':'Cleared to DEST, route DCT DEST, initial altitude 5000 feet, squawk 2105, runway 09, VH-BIL'})
        assert good['result']['accepted'] and good['state']['clearance']['acknowledged'] and 'Custom TOML' in good['result']['message'],good
        tune(121900)
        result=req('start');assert result['result']['accepted'] and result['state']['startupApproved'] and not result['state']['pushbackApproved']
        result=req('pushback');assert result['result']['accepted'] and result['state']['pushbackApproved']
        assert req('start')['result']['accepted']
        result=req('taxi');assert result['result']['accepted'],result
        taxi=result['state']['taxiClearance']
        assert len(taxi['points'])>1 and taxi['pendingReadback'] and not taxi['approved'],result
        assert not any(t['speaker']=='COPILOT' for t in result['state']['transcript']), 'Manual taxi reply must not have a duplicate copilot parrot'
        acknowledged=ok('request/auto-reply', {})
        assert 'altitude' not in acknowledged['result']['message'].lower(),acknowledged
        assert call('request/auto-reply',{})[0]==400
        assert acknowledged['result']['accepted'] and acknowledged['state']['taxiClearance']['approved'],acknowledged
        endpoint=taxi['points'][-1]
        tune(121900,latitude=taxi['referenceLatitude']+endpoint['north']/111320,longitude=taxi['referenceLongitude']+endpoint['east']/(111320*math.cos(math.radians(taxi['referenceLatitude']))),groundSpeedKnots=0)
        assert ok('state')['taxiClearance']['guidanceComplete']
        notice=ok('state')['transcript'][-1]['text'];assert '118.700' in notice and 'Tower' in notice,notice
        # Tower protects the runway against incoming traffic; pause must not issue clearances.
        incoming={'latitude':0,'longitude':-0.02,'altitudeFeet':500,'trackDegrees':90,'onGround':False}
        tune(118700,paused=True,traffic=[incoming]);assert ok('state')['phase']==2
        tune(118700,paused=False,traffic=[incoming])
        held=ok('state');assert held['phase']==2 and held['taxiClearance']['approved'] and 'traffic on final' in held['transcript'][-1]['text'].lower(),held
        count=len(held['transcript']);tune(118700,traffic=[incoming]);assert len(ok('state')['transcript'])==count
        tune(118700,traffic=[]);depart=ok('state');assert depart['phase']==3 and not depart['taxiClearance']['approved'],depart
        assert 'cleared for takeoff' in depart['transcript'][-1]['text'],depart
        tune(121900,latitude=0.003,longitude=-0.003)
        # Radio power and range are unconditional, even for open/emergency requests.
        tune(118700,radioPower=False);assert req('radio_check')['result']['silent']
        tune(118700,radioPower=True,latitude=5.0);assert req('emergency')['result']['silent']
        tune(118000,latitude=0.003);assert call('atis',{})[0]==400
        station=next(s for s in stations if s['service']=='ATIS' and s['airport']=='TEST')
        weather={'airport':'TEST','latitude':station['latitude'],'longitude':station['longitude'],'sampleAltitudeFeet':station['elevationFeet'],'temperatureC':19,'dewpointC':10,'pressureHpa':1005,'windDegrees':270,'windKnots':17,'visibilityMeters':6500,'clouds':'Broken cloud at 2500 feet above airport','source':'simulator-region'}
        ok('weather/simulator',weather)
        report=ok('atis',{});assert report['information']=='Alpha';assert '1005' in report['text'] and '17 knots' in report['text'] and report['source']=='simulator'
        ok('weather/simulator',weather);assert ok('atis',{})['information']=='Alpha'
        weather['pressureHpa']=1006;ok('weather/simulator',weather);assert ok('atis',{})['information']=='Bravo'
        assert call('weather/simulator',dict(weather,sampleAltitudeFeet=10000))[0]==400
        tune(121900);assert call('atis',{})[0]==400
        tune(118100);assert call('atis',{})[0]==400 # no destination sample, never substitute TEST
        dest=next(s for s in stations if s['airport']=='DEST' and s['service']=='ATIS')
        ok('weather/simulator',dict(weather,airport='DEST',latitude=dest['latitude'],longitude=dest['longitude'],sampleAltitudeFeet=0,pressureHpa=1020))
        assert '1020' in ok('atis',{})['text']
        assert ok('weather',{'stations':'DEST'})['source']=='simulator'
        # Reset and enable phrase variety against a deliberately unsafe fake model.
        ok('session/reset',{})
        tune(121800);ok('plan',plan)
        settings=ok('state')['settings'];settings.update(aiEnabled=True,llmPhraseVariety=True,aiModel='fake',aiUrl=f'http://127.0.0.1:{model.server_port}',congestion='off')
        ok('settings',settings)
        result=req('clearance');assert result['result']['accepted'];assert '99000' not in result['result']['message'];assert '5000' in result['result']['message'];assert captured
        assert 'Style examples' in captured[-1]['messages'][0]['content']
        settings['aiEnabled']=False;ok('settings',settings)
        clearance=result['state']['clearance']
        assert req('readback',altitudeFeet=5000,waypoint='DCT DEST',clearanceSequence=clearance['sequence'])['result']['accepted']
        tune(121900)
        combined=req('start_pushback');assert combined['result']['accepted'] and combined['state']['startupApproved'] and combined['state']['pushbackApproved']
        directed=next(t for t in reversed(combined['state']['transcript']) if t['speaker']=='ATC' and not t.get('background',False))
        assert directed['pilotReply'] and plan['callsign'] in directed['pilotReply'],directed
        acknowledgment={'intent':'acknowledge','role':'atc','text':directed['pilotReply'],'clearanceSequence':directed['sequence']}
        assert ok('request',acknowledgment)['result']['accepted']
        assert not ok('request',acknowledgment)['result']['accepted'], 'An acknowledgment must not be reusable'
        # No taxi graph or hold marking: Ground must hand off, Tower grants runway taxi.
        ok('session/reset',{})
        runway_plan=dict(plan,departure='RWYO')
        ok('plan',runway_plan)
        tune(122000,latitude=0.0003,longitude=0.008,groundSpeedKnots=0)
        cleared=req('clearance');assert cleared['result']['accepted'],cleared
        pending=cleared['state']['clearance']
        assert req('readback',altitudeFeet=pending['altitudeFeet'],waypoint=pending['route'],clearanceSequence=pending['sequence'])['result']['accepted']
        ground=req('taxi');assert not ground['result']['accepted'] and 'Tower' in ground['result']['message'],ground
        tune(122100)
        runway=req('backtrack');assert runway['result']['accepted'],runway
        path=runway['state']['taxiClearance'];assert path['runwayTaxi'] and path['pendingReadback'] and not path['approved'],path
        accepted=ok('request/auto-reply', {})
        assert 'altitude' not in accepted['result']['message'].lower(),accepted
        assert accepted['result']['accepted'] and accepted['state']['taxiClearance']['approved'],accepted
        assert not req('ready')['result']['accepted'], 'Cannot be ready before reaching the runway end'
        endpoint=path['points'][-1]
        tune(122100,latitude=path['referenceLatitude']+endpoint['north']/111320,longitude=path['referenceLongitude']+endpoint['east']/(111320*math.cos(math.radians(path['referenceLatitude']))))
        assert ok('state')['taxiClearance']['guidanceComplete']
        assert ok('state')['phase']==3, 'Tower automatically clears departure after runway taxi is complete and traffic is clear'
        tune(123450)
        before=len(captured);assert req('conversation')['result']['silent'];assert len(captured)==before
        # Copilot readback mode consumes each clearance once with complete pilot facts.
        ok('session/reset',{})
        settings=ok('state')['settings'];settings.update(copilotReplies=True,copilotAutoRespond=True,congestion='off');ok('settings',settings)
        tune(121800,latitude=0.003,longitude=-0.003,traffic=[]);ok('plan',plan)
        automatic=req('clearance');assert automatic['state']['clearance']['acknowledged'],automatic
        tune(121900);automatic=req('taxi');assert automatic['state']['taxiClearance']['approved'] and not automatic['state']['taxiClearance']['pendingReadback'],automatic
        copilot=[t for t in automatic['state']['transcript'] if t['speaker']=='COPILOT'];assert copilot and all(not t['position'] for t in copilot),copilot
        assert all('Read back' not in t['text'] for t in copilot),copilot
        assert not any(t.get('background',False) for t in ok('state')['transcript']), 'Live sessions must not fabricate background aircraft'
        print('Radio engine: station roles, silence, clearance/readback/start/pushback/taxi, reusable-ack rejection, Tower runway-backtrack/readback/endpoint gating, airport weather, ATIS letters/destination, unsafe LLM fallback passed')
    finally:
        engine.terminate();engine.wait(timeout=5);log.close();model.shutdown()
