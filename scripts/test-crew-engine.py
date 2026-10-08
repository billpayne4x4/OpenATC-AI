"""Exercise aircraft crew actions and both checklist modes against the real engine."""
import argparse, json, os, socket, subprocess, tempfile, time, tomllib, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.request import Request, urlopen
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('engine',type=Path)
args=parser.parse_args()
repo=Path(__file__).resolve().parent.parent
profile=tomllib.loads((repo/'aircraft/toliss_a320.toml').read_text())['aircraft']['crew']
with socket.socket() as sock:
    sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
def call(path,body=None):
    request=Request(f'http://127.0.0.1:{port}/{path}',data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
    with urlopen(request,timeout=5) as response:return json.load(response)
with tempfile.TemporaryDirectory(prefix='openatc-crew-') as temporary:
    env=dict(os.environ,OPENATC_CONFIG_DIR=temporary)
    settings={'aiEnabled':False,'speechDir':str(repo/'speech')}
    Path(temporary,'settings.json').write_text(json.dumps(settings))
    log=open(Path(temporary,'engine.log'),'w')
    process=subprocess.Popen([str(args.engine.resolve()),str(port)],cwd=repo,env=env,stdout=log,stderr=log)
    try:
        for _ in range(60):
            try:call('health');break
            except OSError:time.sleep(.1)
        else:raise AssertionError('engine did not start')
        call('telemetry',{'latitude':17.974,'longitude':102.57,'onGround':True,'positionValid':True,'radioPower':True,'com1Khz':118100,'groundSpeedKnots':0})
        values={'heading':0,'altitude':5000,'gear':1,'beacon':0,'seatbelts':0,'slides':0,'cabin_brightness':100,'chocks':0}
        def observe(aircraft='ToLiss neo'):
            return call('crew/observe',{'aircraft':aircraft,'profile':profile,'values':values,'available':list(profile['controls'])})
        def request(role,text):
            observe();return call('request',{'role':role,'intent':'conversation','text':text})
        def action(role,text,control,value):
            result=request(role,text);assert result['result']['pending'],result
            pending=result['state']['crewActions'];assert len(pending)==1,pending
            task=pending[0];assert task['control']==control and task['value']==value,task
            assert not any(row['text'].startswith('Heading,') for row in result['state']['transcript'][-1:]),result
            ack={'sequence':task['sequence'],'aircraft':task['aircraft'],'success':True,'detail':'test control readback'}
            completed=call('crew/ack',ack);assert completed['accepted'],completed
            transcript=call('state')['transcript'];assert transcript[-1]['speaker'] in ['COPILOT','ATTENDANT','GROUND'],transcript[-1]
            count=len(transcript);assert not call('crew/ack',ack)['accepted'];assert len(call('state')['transcript'])==count
            values[control]=value;return completed
        action('copilot','set heading 270','heading',270)
        action('copilot','set heading 360','heading',0)
        action('copilot','set altitude flight level 240','altitude',24000)
        action('ground','add chocks','chocks',1)
        action('ground','remove chocks','chocks',0)
        action('ground','Remove chalks.','chocks',0)
        compound=request('ground','disconnect external power and shocks');assert not compound['state']['crewActions'] and not compound['result']['accepted'],compound
        action('cabin','cabin brightness 50 percent','cabin_brightness',50)
        armed=action('cabin','arm slides and crosscheck','slides',1)
        assert 'armed and cross-checked' in armed['state']['transcript'][-1]['text'].lower(),armed
        query=request('cabin','crosscheck');assert query['result']['accepted'] and not query['state']['crewActions']
        action('cabin','doors to manual','slides',0)
        query=request('copilot','is beacon on?');assert not query['state']['crewActions']
        no=request('copilot',"don't turn beacon on");assert not no['state']['crewActions']
        for role,text in [('copilot','gear up'),('copilot','set heading 999'),('cabin','set heading 270')]:
            denied=request(role,text);assert not denied['result']['accepted'] and not denied['state']['crewActions'],denied
        # Checklists wait for the captain and reject a response inconsistent with actual state.
        started=request('copilot','read before start checklist');assert 'Seat belt' in started['result']['message'],started
        mismatch=request('copilot','on');assert not mismatch['result']['accepted'],mismatch
        values['seatbelts']=1
        advanced=request('copilot','on');assert 'Beacon' in advanced['result']['message'],advanced
        values['beacon']=1
        finished=request('copilot','set');assert 'complete' in finished['result']['message'],finished
        # Perform mode queues one item at a time, and only advances after confirmation.
        values['seatbelts']=0;values['beacon']=0
        started=request('copilot','perform before start checklist');task=started['state']['crewActions'][0];assert task['control']=='seatbelts'
        response=call('crew/ack',{'sequence':task['sequence'],'aircraft':task['aircraft'],'success':True,'detail':'checked'})
        task=response['state']['crewActions'][0];assert task['control']=='beacon'
        response=call('crew/ack',{'sequence':task['sequence'],'aircraft':task['aircraft'],'success':False,'detail':'unavailable'})
        assert not response['state']['crewActions'];assert 'not' in response['state']['transcript'][-1]['text'].lower()
        # A catalogued button produces a dispatched-button reply, not a claim about its system.
        button=next((id,c) for id,c in profile['controls'].items() if c.get('momentary'))
        # The label is one of the request anchors for these catalogued commands.
        result=request('copilot','please '+button[1]['label'].lower());task=result['state']['crewActions'][0]
        response=call('crew/ack',{'sequence':task['sequence'],'aircraft':task['aircraft'],'success':True,'detail':'command dispatched'})
        assert 'button pressed' in response['state']['transcript'][-1]['text'].lower(),response
        greeting=request('cabin','How are you?');assert greeting['result']['accepted'] and 'landing' not in greeting['result']['message'].lower(),greeting
        action('ground','start pushback','pushback_start',1)
        for text in ["don't start pushback", 'stop pushback', 'can you start pushback?']:
            denied=request('ground',text);assert not denied['state']['crewActions'],denied
        # Tug requests use parameters already configured in the aircraft.
        action('ground','request pushback 80 meters 60 degrees right','pushback_start',1)
        unknown=request('ground','connect high pressure air');assert not unknown['result']['accepted'] and not unknown['state']['crewActions'],unknown
        # Free-form interpretation returns bounded JSON; it cannot invent a ref or exceed limits.
        model_answer={"control":"heading","value":275}
        model_calls=[]
        class Model(BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_POST(self):
                model_calls.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
                body=json.dumps({'choices':[{'message':{'content':json.dumps(model_answer)}}]}).encode()
                self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
        model=ThreadingHTTPServer(('127.0.0.1',0),Model)
        threading.Thread(target=model.serve_forever,daemon=True).start()
        settings=call('state')['settings'];settings.update(aiEnabled=True,aiModel='test',aiUrl=f'http://127.0.0.1:{model.server_port}')
        call('settings',settings)
        action('copilot','please choose heading two seven five','heading',275)
        assert model_calls and 'Pilot wording examples' in model_calls[-1]['messages'][0]['content']
        for proposed in [{'control':'heading','value':999},{'control':'slides','value':1},{'control':'sim/custom/anything','value':1}]:
            model_answer.clear();model_answer.update(proposed)
            denied=request('copilot','please choose heading two seven five');assert not denied['result']['accepted'] and not denied['state']['crewActions'],denied
        settings.update(aiEnabled=False);call('settings',settings);model.shutdown();model.server_close()
        # Aircraft replacement invalidates queued controls and checklist progress.
        queued=request('copilot','beacon on');assert queued['state']['crewActions']
        action=queued['state']['crewActions'][0]
        cleared=call('session/reset',{})
        assert not cleared['crewActions'] and not cleared['transcript'],cleared
        assert not call('crew/ack',dict(sequence=action['sequence'],aircraft=action['aircraft'],success=True,detail=''))['accepted']
        observe('different aircraft');assert not call('state')['crewActions']
        print('CREW_ENGINE_OK: roles, bounded targets, confirmations, idempotent ack, checklist modes, button dispatch and aircraft replacement')
    finally:
        process.terminate()
        try:process.wait(timeout=5)
        except subprocess.TimeoutExpired:process.kill();process.wait()
        log.close()
