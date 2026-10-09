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
slow_started=threading.Event()
slow_release=threading.Event()
slow_model=False
class Model(BaseHTTPRequestHandler):
    def log_message(self,*args): pass
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        if self.path.endswith('/audio/speech'):
            self.send_response(503);self.end_headers();self.wfile.write(b'test provider unavailable')
            return
        captured.append(body)
        if slow_model:
            slow_started.set()
            slow_release.wait(timeout=10)
        if body.get('messages',[{}])[0].get('content','').startswith('Classify the pilot message'):
            data=json.dumps({'choices':[{'message':{'content':json.dumps({'intent':'taxi','altitudeFeet':0,'waypoint':''})}}]}).encode()
            self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
            return
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
    parallel = ('1 0 0 0 PARA Parallel Runways\n'
        '100 45 1 1 0.25 0 3 0 25R 0 0 0 0 2 0 0 0 07L 0 0.02 0 0 2 0 0 0\n'
        '100 45 1 1 0.25 0 3 0 25L -0.002 0 0 0 2 0 0 0 07R -0.002 0.02 0 0 2 0 0 0\n'
        '1053 124500 Parallel Ground\n1054 124600 Parallel Tower\n'
        '1201 0.003 0.002 both 1\n1201 0.001 0.002 both 2\n1201 -0.001 0.002 both 3\n1201 -0.001 0 both 4\n'
        '1202 1 2 twoway taxiway_C Alpha\n1202 2 3 twoway taxiway_C Alpha\n1204 departure 25R,07L\n1202 3 4 twoway taxiway_C Bravo\n')
    apt.write_text(apt.read_text().replace('99\n', parallel+'99\n'))
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
        stations=ok('stations/nearby',{})['stations'];assert len(stations)==12,stations
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
        assert rejected['state']['transcript'][-1]['text'].endswith(rejected['result']['message']),rejected
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
        result=req('conversation',text=f"Test Ground, {plan['callsign']}, reqest taxy");assert result['result']['accepted'],result
        taxi=result['state']['taxiClearance']
        assert len(taxi['points'])>1 and taxi['pendingReadback'] and not taxi['approved'],result
        assert not any(t['speaker']=='COPILOT' for t in result['state']['transcript']), 'Manual taxi reply must not have a duplicate copilot parrot'
        acknowledged=ok('request/auto-reply', {})
        assert 'altitude' not in acknowledged['result']['message'].lower(),acknowledged
        assert call('request/auto-reply',{})[0]==400
        assert acknowledged['result']['accepted'] and acknowledged['state']['taxiClearance']['approved'],acknowledged
        endpoint=taxi['points'][-1]
        tune(121900,latitude=taxi['referenceLatitude']+(endpoint['north']+35)/111320,longitude=taxi['referenceLongitude']+endpoint['east']/(111320*math.cos(math.radians(taxi['referenceLatitude']))),groundSpeedKnots=0)
        assert ok('state')['taxiClearance']['guidanceComplete']
        notice=ok('state')['transcript'][-1]['text'];assert '118.700' in notice and 'Tower' in notice,notice
        # Tower protects the runway against incoming traffic; pause must not issue clearances.
        incoming={'latitude':0,'longitude':-0.02,'altitudeFeet':500,'trackDegrees':90,'onGround':False}
        tune(118700,paused=True,traffic=[incoming]);assert ok('state')['phase']==2
        tune(118700,paused=False,traffic=[incoming])
        assert ok('state')['phase']==2
        req('ready')
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
        tune(121800)
        positioned=req('position');assert positioned['result']['accepted']
        assert '0.003000' not in positioned['result']['message']
        assert '102.' not in positioned['result']['message']
        # Reset and enable phrase variety against a deliberately unsafe fake model.
        ok('session/reset',{})
        tune(121800);ok('plan',plan)
        settings=ok('state')['settings'];settings.update(aiEnabled=True,llmPhraseVariety=True,aiModel='fake',aiUrl=f'http://127.0.0.1:{model.server_port}',congestion='off')
        settings['llmPhraseVariety']=False
        ok('settings',settings)
        before=len(captured)
        strict=req('clearance');assert strict['result']['accepted'];assert 'read back' not in strict['result']['message'].lower()
        assert len(captured)==before, 'strict phrases must not reach the wording model'
        ok('session/reset',{});tune(121800);ok('plan',plan)
        strict_next=req('clearance');assert strict_next['result']['accepted']
        assert strict_next['result']['message'] != strict['result']['message'], 'Clearance wording must rotate across flight resets'
        assert len(captured)==before
        clearance_wordings=set()
        for _ in range(10):
            ok('session/reset',{});tune(121800);ok('plan',plan)
            strict_next=req('clearance');assert strict_next['result']['accepted']
            clearance_wordings.add(strict_next['result']['message'])
        assert len(clearance_wordings)==10,clearance_wordings
        assert len(captured)==before
        history=json.loads((config/'phrase-history.json').read_text())
        assert strict_next['result']['message'] in history['last'].values(),history
        ok('session/reset',{});tune(121800);ok('plan',plan)
        settings['llmPhraseVariety']=True
        ok('settings',settings)
        result=req('clearance');assert result['result']['accepted'];assert '99000' not in result['result']['message'];assert '5000' in result['result']['message'];assert captured
        wording_prompt=captured[-1]['messages'][0]['content']
        assert 'Style examples' in wording_prompt
        assert 'clearance-delivery controller' in wording_prompt
        assert 'current task is clearance' in wording_prompt
        assert strict_next['result']['message'] in wording_prompt,wording_prompt
        settings['ttsUrl']=f'http://127.0.0.1:{model.server_port}';ok('settings',settings)
        failed_phrase='Position approximately three miles northwest of Vientiane.'
        assert call('speech/speak', {'text':failed_phrase,'speaker':'atc'})[0]==400
        diagnostic=(folder/'engine.log').read_text()
        assert failed_phrase in diagnostic and '503' in diagnostic, diagnostic
        settings['aiEnabled']=False;ok('settings',settings)
        clearance=result['state']['clearance']
        readback_result=req('readback',altitudeFeet=5000,waypoint='DCT DEST',clearanceSequence=clearance['sequence']);assert readback_result['result']['accepted'];assert '121.900' in readback_result['result']['message'] and 'when ready' in readback_result['result']['message'].lower(),readback_result
        delivery_voice=next(t['voice'] for t in reversed(readback_result['state']['transcript']) if t['speaker']=='ATC')
        tune(121900)
        combined=req('start_pushback');assert combined['result']['accepted'] and combined['state']['startupApproved'] and combined['state']['pushbackApproved']
        ground_voice=next(t['voice'] for t in reversed(combined['state']['transcript']) if t['speaker']=='ATC')
        assert ground_voice != delivery_voice, 'Ground and Delivery must differ while unused English voices remain'
        directed=next(t for t in reversed(combined['state']['transcript']) if t['speaker']=='ATC' and not t.get('background',False))
        assert directed['pilotReply'] and plan['callsign'] in directed['pilotReply'],directed
        acknowledgment={'intent':'acknowledge','role':'atc','text':directed['pilotReply'],'clearanceSequence':directed['sequence']}
        assert ok('request',acknowledgment)['result']['accepted']
        assert not ok('request',acknowledgment)['result']['accepted'], 'An acknowledgment must not be reusable'
        repeated=req('radio_check')
        assert any(t['speaker']=='ATC' for t in repeated['state']['transcript']),repeated
        assert next(t['voice'] for t in reversed(repeated['state']['transcript']) if t['speaker']=='ATC') == ground_voice
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
        assert ok('state')['phase']==2, 'Reaching the holding point does not report ready for the pilot'
        assert req('ready')['result']['accepted']
        assert ok('state')['phase']==3
        count=len(ok('state')['transcript'])
        for _ in range(5): tune(122100)
        stationary=ok('state')
        assert len(stationary['transcript'])==count, 'No new handoff while stopped with takeoff clearance'
        assert all(plan['callsign'] in t['text'] for t in stationary['transcript'] if t['speaker']=='ATC' and not t.get('background'))
        settings=ok('state')['settings'];settings['copilotReplies']=True;ok('settings',settings)
        ok('request/copilot-prepare',{});ok('request/copilot-reply',{})
        takeoff_reply=ok('state')['transcript'][-1]
        assert takeoff_reply['speaker']=='COPILOT' and 'cleared for takeoff' in takeoff_reply['text'].lower(),takeoff_reply
        settings['copilotReplies']=False;ok('settings',settings)
        repeat_clearance=req('clearance')
        assert not repeat_clearance['result']['accepted'] and 'contact' not in repeat_clearance['result']['message'].lower(),repeat_clearance

        tune(122100,onGround=False,altitudeFeet=1500,groundSpeedKnots=180)
        checkin=req('checkin',text=f"Tower, {plan['callsign']}, 1500 feet.")
        assert checkin['result']['accepted'] and '1500' in checkin['result']['message'],checkin

        tune(123450)
        before=len(captured);assert req('conversation')['result']['silent'];assert len(captured)==before
        # Copilot readback mode consumes each clearance once with complete pilot facts.
        ok('session/reset',{})
        settings=ok('state')['settings'];settings.update(copilotReplies=True,copilotAutoRespond=True,congestion='off');ok('settings',settings)
        tune(121800,latitude=0.003,longitude=-0.003,traffic=[],onGround=True,groundSpeedKnots=0);ok('plan',plan)
        automatic=req('clearance');assert not automatic['state']['clearance']['acknowledged'],automatic
        ok('request/copilot-prepare',{});assert not ok('state')['clearance']['acknowledged']
        ok('request/copilot-reply',{});assert ok('state')['clearance']['acknowledged']
        tune(121900);automatic=req('taxi');assert automatic['state']['taxiClearance']['pendingReadback'],automatic
        ok('request/copilot-prepare',{});ok('request/copilot-reply',{});automatic={'state':ok('state')}
        assert automatic['state']['taxiClearance']['approved']
        copilot=[t for t in automatic['state']['transcript'] if t['speaker']=='COPILOT'];assert copilot and all(not t['position'] for t in copilot),copilot
        assert all('Read back' not in t['text'] for t in copilot),copilot
        assert not any(t.get('background',False) for t in ok('state')['transcript']), 'Live sessions must not fabricate background aircraft'
        # Ground proactively clears the actual intervening runway, then resumes taxi.
        ok('session/reset',{})
        settings=ok('state')['settings'];settings.update(copilotReplies=False,copilotAutoRespond=False);ok('settings',settings)
        tune(124500,latitude=0.003,longitude=0.002,traffic=[],groundSpeedKnots=0)
        ok('plan',dict(plan,departure='PARA',runway='25L'))
        cleared=req('clearance');assert cleared['result']['accepted'],cleared
        pending=cleared['state']['clearance']
        assert req('readback',altitudeFeet=pending['altitudeFeet'],waypoint=pending['route'],clearanceSequence=pending['sequence'])['result']['accepted']
        taxi=req('taxi');assert taxi['result']['accepted'],taxi
        assert taxi['state']['taxiClearance']['holdShortRunway']=='25R',taxi
        assert ok('request/auto-reply',{})['result']['accepted']
        before_hold=len(ok('state')['transcript'])
        tune(124500,latitude=0.001,traffic=[],groundSpeedKnots=8,radioBusy=True)
        assert len(ok('state')['transcript'])==before_hold, 'ATC must wait while pilot or copilot occupies the radio'
        tune(124500,radioBusy=False,radioSequenceSeen=0)
        assert len(ok('state')['transcript'])==before_hold, 'Wait until the simulator has observed the preceding ATC message'
        tune(124500,latitude=0.001,traffic=None,radioBusy=False,radioSequenceSeen=None)

        held=ok('state')['taxiClearance']
        assert not held['crossingRunway'] and held['waitingForTraffic'],held
        tune(124500,latitude=0.001,traffic=[{'latitude':0,'longitude':0.002,'altitudeFeet':0,'onGround':True,'trackDegrees':270,'groundSpeedKnots':5}])
        assert not ok('state')['taxiClearance']['crossingRunway'], 'An occupied runway must not be cleared for crossing'
        tune(124500,traffic=[],groundSpeedKnots=8)
        crossing=ok('state');assert crossing['taxiClearance']['crossingRunway']=='25R',crossing
        assert crossing['taxiClearance']['destination']=='25L'
        assert crossing['taxiClearance']['pendingReadback'] and not crossing['taxiClearance']['approved']
        assert not any(t['speaker']==plan['callsign'] and t['text']=='cross_runway' for t in crossing['transcript'])
        assert not req('ready')['result']['accepted']
        before_crossing_readback=len([t for t in ok('state')['transcript'] if t['speaker']=='ATC'])
        crossing_ack=ok('request/auto-reply',{})
        assert crossing_ack['result']['accepted'] and not crossing_ack['result']['message'],crossing_ack
        assert len([t for t in crossing_ack['state']['transcript'] if t['speaker']=='ATC'])==before_crossing_readback
        assert crossing_ack['state']['taxiClearance']['approved']
        tune(124500,latitude=0)
        assert ok('state')['taxiClearance']['crossingRunway']=='25R', 'Crossing must not finish on the runway'
        tune(124500,latitude=-0.0012,groundSpeedKnots=8)
        onward=ok('state')['taxiClearance']
        assert not onward['crossingRunway'] and onward['holdShortRunway']=='25L',onward
        assert onward['pendingReadback'] and not onward['approved']
        vacated=req('conversation',text='VACTED')
        assert vacated['result']['message']==onward['instructions'],vacated
        assert 'readback correct' not in vacated['result']['message'].lower()
        assert ok('request/auto-reply',{})['result']['accepted']
        endpoint=onward['points'][-1]
        tune(124500,latitude=onward['referenceLatitude']+endpoint['north']/111320,
             longitude=onward['referenceLongitude']+endpoint['east']/(111320*math.cos(math.radians(onward['referenceLatitude']))),groundSpeedKnots=0)
        held=ok('state')
        assert held['taxiClearance']['guidanceComplete'],held
        assert any('124.600' in t['text'] and 'Tower' in t['text'] for t in held['transcript']),held

        controller_entries=[t for t in ok('state')['transcript'] if t['speaker']=='ATC']
        assert all(t['voice'] in ['alloy','echo','fable','onyx','nova','shimmer'] for t in controller_entries),controller_entries
        previous=ok('state')
        cleared=ok('session/reset',{})
        assert not cleared['transcript'] and not cleared['plan']['departure'] and not cleared['plan']['destination'],cleared
        assert cleared['clearance'] is None and not cleared['taxiClearance']['approved'] and not cleared['taxiClearance']['points'],cleared
        assert not cleared['crewActions'] and not cleared['recommendedFrequencyKhz'],cleared
        assert not cleared['startupApproved'] and not cleared['pushbackApproved'],cleared
        assert cleared['telemetry']['latitude']==previous['telemetry']['latitude']
        assert cleared['nextSequence']==previous['nextSequence'], 'Old transmission IDs must not be reused'
        assert ok('session/reset',{})['transcript']==[]
        settings=ok('state')['settings'];settings.update(aiEnabled=True,llmPhraseVariety=True);ok('settings',settings)
        tune(121800,latitude=0.003,longitude=-0.003,traffic=[],groundSpeedKnots=0);ok('plan',plan)
        slow_model=True
        delayed=[]
        worker=threading.Thread(target=lambda: delayed.append(call('request',{'intent':'clearance','text':'Request IFR clearance','role':'atc'})))
        worker.start();assert slow_started.wait(timeout=5), 'model request did not start'
        assert ok('session/reset',{})['transcript']==[]
        slow_release.set();worker.join(timeout=10)
        assert delayed and delayed[0][0]==400,delayed
        assert ok('state')['transcript']==[], 'Late model output must not restore the old conversation'
        # Saved English assignments survive a complete engine restart.
        roster=json.loads((config/'controllers.json').read_text())
        settings=ok('state')['settings'];settings.update(aiEnabled=False);ok('settings',settings)
        engine.terminate();engine.wait(timeout=5)
        engine=subprocess.Popen([str(args.engine.resolve()),str(port)],env=env,stdout=log,stderr=log)
        for attempt in range(100):
            try:
                if call('health')[0]==200:break
            except URLError:time.sleep(.05)
        tune(121900,latitude=0.003,longitude=-0.003,traffic=[])
        ok('plan',plan)
        ok('stations/nearby',{})
        repeated=req('radio_check')
        assert any(t['speaker']=='ATC' for t in repeated['state']['transcript']),repeated
        assert next(t['voice'] for t in reversed(repeated['state']['transcript']) if t['speaker']=='ATC')==ground_voice
        assert json.loads((config/'controllers.json').read_text())==roster
        print('Radio engine: station roles, silence, clearance/readback/start/pushback/taxi, reusable-ack rejection, Tower runway-backtrack/readback/endpoint gating, proactive Ground crossing/traffic hold/readback/onward taxi, airport weather, ATIS letters/destination, phrase rotation/history, role-aware variety prompts, unsafe LLM fallback passed')
    finally:
        engine.terminate();engine.wait(timeout=5);log.close();model.shutdown()
