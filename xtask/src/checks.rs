mod crew;
mod radio;
mod speech;
use crate::{Result, distribution::tree, repo};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
struct Model {
    port: u16,
    requests: Arc<Mutex<Vec<Value>>>,
    answer: Arc<Mutex<String>>,
    slow: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Model {
    fn new(answer: &str) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let answer = Arc::new(Mutex::new(answer.to_owned()));
        let slow = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (r, a, s, b, g, q) = (
            requests.clone(),
            answer.clone(),
            slow.clone(),
            started.clone(),
            release.clone(),
            stop.clone(),
        );
        let worker = thread::spawn(move || {
            while !q.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let (r, a, s, b, g) =
                            (r.clone(), a.clone(), s.clone(), b.clone(), g.clone());
                        thread::spawn(move || {
                            if let Err(e) = serve(stream, r, a, s, b, g) {
                                eprintln!("Fake model: {e}");
                            }
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            port,
            requests,
            answer,
            slow,
            started,
            release,
            stop,
            worker: Some(worker),
        })
    }
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    fn prompt(&self) -> String {
        self.requests.lock().unwrap().last().unwrap()["messages"][0]["content"]
            .as_str()
            .unwrap()
            .into()
    }
    fn set(&self, value: Value) {
        *self.answer.lock().unwrap() = value.to_string();
    }
}
impl Drop for Model {
    fn drop(&mut self) {
        self.release.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.worker.take() {
            let _ = t.join();
        }
    }
}
fn serve(
    mut stream: TcpStream,
    requests: Arc<Mutex<Vec<Value>>>,
    answer: Arc<Mutex<String>>,
    slow: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
) -> Result {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut data = Vec::new();
    let mut byte = [0];
    while !data.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte)?;
        data.push(byte[0]);
        if data.len() > 65536 {
            return Err("Oversized HTTP headers".into());
        }
    }
    let header = String::from_utf8(data)?;
    let length = header
        .lines()
        .find_map(|l| {
            l.to_lowercase()
                .strip_prefix("content-length:")
                .and_then(|s| s.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    if length > 8 * 1024 * 1024 {
        return Err("Oversized mock request".into());
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body)?;
    let (status, body) = if header
        .lines()
        .next()
        .unwrap_or("")
        .contains("/audio/speech")
    {
        (503, "test provider unavailable".into())
    } else {
        let request: Value = serde_json::from_slice(&body)?;
        requests.lock().unwrap().push(request.clone());
        if slow.load(Ordering::SeqCst) {
            started.store(true, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !release.load(Ordering::SeqCst) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
        }
        let content = if request["messages"][0]["content"]
            .as_str()
            .unwrap_or("")
            .starts_with("Classify the pilot message")
        {
            json!({"intent":"taxi","altitudeFeet":0,"waypoint":""}).to_string()
        } else {
            answer.lock().unwrap().clone()
        };
        (
            200,
            json!({"choices":[{"message":{"content":content}}]}).to_string(),
        )
    };
    write!(
        stream,
        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    Ok(())
}
struct Engine {
    child: Child,
    client: Client,
    url: String,
    config: std::path::PathBuf,
    folder: tempfile::TempDir,
    telemetry: Value,
    executable: std::path::PathBuf,
    speech: std::path::PathBuf,
    model_url: String,
}
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Engine {
    fn new(binary: &Path, speech: &Path, model: &Model, radio: bool) -> Result<Self> {
        let folder = tempfile::tempdir()?;
        let config = folder.path().join("config");
        fs::create_dir(&config)?;
        let library = folder.path().join("speech");
        tree(&speech.canonicalize()?, &library)?;
        let sim = folder.path().join("sim");
        let apt = sim.join("Global Scenery/Global Airports/Earth nav data/apt.dat");
        fs::create_dir_all(apt.parent().unwrap())?;
        fs::write(apt, include_str!("../fixtures/radio-apt.dat"))?;
        let unrelated = sim.join("Custom Scenery/Unrelated/Earth nav data");
        fs::create_dir_all(&unrelated)?;
        fs::write(
            unrelated.join("apt.dat"),
            "1 0 0 0 OTHER Unrelated scenery\n99\n",
        )?;
        fs::write(
            sim.join("Custom Scenery/scenery_packs.ini"),
            "SCENERY_PACK Custom Scenery/Unrelated/\n",
        )?;
        if radio {
            let runtime = library.join("runtime/responses.toml");
            let mut text = fs::read_to_string(&runtime)?;
            for (id, phrase) in [
                (
                    "readback_correct_for_recorded_altitude_and_route",
                    "Custom TOML readback accepted for recorded altitude and route.",
                ),
                (
                    "airport_identifier_not_found_in_apt_dat",
                    "Custom edited missing airport wording.",
                ),
            ] {
                let begin = text
                    .find(&format!("[{id}]"))
                    .ok_or("Missing runtime phrase")?;
                let end = text[begin + 1..]
                    .find("\n[")
                    .map_or(text.len(), |offset| begin + 1 + offset);
                text.replace_range(
                    begin..end,
                    &format!("[{id}]\nslots = []\nsay = ['{phrase}']\n"),
                );
            }
            fs::write(runtime, text)?;
        }
        fs::write(config.join("settings.json"),json!({"simulatorRoot":sim,"aiEnabled":false,"copilotReplies":false,"copilotAutoRespond":true,"congestion":"off","strictReadbacks":true,"speechDir":library}).to_string())?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let executable = binary.canonicalize()?;
        let child = spawn(
            &executable,
            port,
            &config,
            &library,
            &model.url(),
            folder.path(),
        )?;
        let mut engine = Self {
            child,
            client: Client::builder().timeout(Duration::from_secs(35)).build()?,
            url: format!("http://127.0.0.1:{port}"),
            config,
            folder,
            telemetry: json!({"latitude":0.003,"longitude":-0.003,"altitudeFeet":0,"onGround":true,"positionValid":true,"radioPower":true,"com1Khz":123450,"traffic":[]}),
            executable,
            speech: library,
            model_url: model.url(),
        };
        engine.ready()?;
        Ok(engine)
    }
    fn ready(&mut self) -> Result {
        for _ in 0..100 {
            if self
                .call("health", None)
                .is_ok_and(|(status, _)| status == 200)
            {
                return Ok(());
            }
            if self.child.try_wait()?.is_some() {
                return Err(format!(
                    "Engine exited:\n{}",
                    fs::read_to_string(self.folder.path().join("engine.log"))?
                )
                .into());
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err("Engine did not become ready".into())
    }
    fn call(&self, path: &str, body: Option<Value>) -> Result<(u16, Value)> {
        let url = format!("{}/{path}", self.url);
        let response = match body {
            Some(v) => self.client.post(url).json(&v).send()?,
            None => self.client.get(url).send()?,
        };
        let status = response.status().as_u16();
        let text = response.text()?;
        Ok((
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        ))
    }
    fn ok(&self, path: &str, body: Option<Value>) -> Result<Value> {
        let (status, v) = self.call(path, body)?;
        if status != 200 {
            return Err(format!("/{path} returned {status}: {v}").into());
        }
        Ok(v)
    }
    fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.ok(path, Some(body))
    }
    fn state(&self) -> Result<Value> {
        self.ok("state", None)
    }
    fn req(&self, intent: &str) -> Result<Value> {
        self.post(
            "request",
            json!({"intent":intent,"text":intent,"role":"atc"}),
        )
    }
    fn tune(&mut self, freq: u32, changes: Value) -> Result {
        self.telemetry["com1Khz"] = json!(freq);
        merge(&mut self.telemetry, changes);
        self.post("telemetry", self.telemetry.clone())?;
        Ok(())
    }
    fn settings(&self, changes: Value) -> Result {
        let mut value = self.state()?["settings"].clone();
        merge(&mut value, changes);
        self.post("settings", value)?;
        Ok(())
    }
    fn reset(&self) -> Result<Value> {
        self.post("session/reset", json!({}))
    }
    fn endpoint(&mut self, freq: u32, taxi: &Value, offset: f64, speed: f64) -> Result {
        let lat = taxi["referenceLatitude"].as_f64().unwrap();
        let lon = taxi["referenceLongitude"].as_f64().unwrap();
        let point = taxi["points"].as_array().unwrap().last().unwrap();
        self.tune(freq,json!({"latitude":lat+(point["north"].as_f64().unwrap()+offset)/111320.,"longitude":lon+point["east"].as_f64().unwrap()/(111320.*lat.to_radians().cos()),"groundSpeedKnots":speed}))
    }
    fn restart(&mut self) -> Result {
        self.child.kill()?;
        self.child.wait()?;
        let port = self.url.rsplit(':').next().unwrap().parse()?;
        self.child = spawn(
            &self.executable,
            port,
            &self.config,
            &self.speech,
            &self.model_url,
            self.folder.path(),
        )?;
        self.ready()
    }
}
fn spawn(
    binary: &Path,
    port: u16,
    config: &Path,
    speech: &Path,
    model: &str,
    folder: &Path,
) -> Result<Child> {
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(folder.join("engine.log"))?;
    Ok(Command::new(binary)
        .arg(port.to_string())
        .current_dir(repo())
        .env("OPENATC_CONFIG_DIR", config)
        .env("OPENATC_SPEECH_DIR", speech)
        .env("OPENATC_AI_URL", model)
        .env("OPENATC_AI_MODEL", "fake")
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()?)
}
fn merge(base: &mut Value, changes: Value) {
    for (key, value) in changes.as_object().unwrap() {
        base[key] = value.clone();
    }
}
fn accepted(v: &Value) {
    assert_eq!(v["result"]["accepted"], true, "{v}");
}
fn rejected(v: &Value) {
    assert_eq!(v["result"]["accepted"], false, "{v}");
}
fn contains(v: &Value, text: &str) {
    assert!(
        v["result"]["message"].as_str().unwrap_or("").contains(text),
        "Expected {text:?}: {v}"
    );
}
fn latest(state: &Value) -> String {
    state["transcript"].as_array().unwrap().last().unwrap()["text"]
        .as_str()
        .unwrap()
        .into()
}
fn count(state: &Value) -> usize {
    state["transcript"].as_array().unwrap().len()
}
fn plan() -> Value {
    json!({"departure":"TEST","destination":"DEST","callsign":"VH-BIL","runway":"09","arrivalRunway":"09","initialAltitudeFeet":5000,"cruiseFeet":25000,"route":"DCT DEST"})
}
fn clearance(e: &Engine) -> Result {
    let v = e.req("clearance")?;
    accepted(&v);
    let c = &v["state"]["clearance"];
    accepted(&e.post("request",json!({"intent":"readback","role":"atc","altitudeFeet":c["altitudeFeet"],"waypoint":c["route"],"clearanceSequence":c["sequence"]}))?);
    Ok(())
}
pub(crate) fn exercise(kind: &str, binary: &Path, speech: &Path) -> Result {
    let model = Model::new("Cleared to FAKE. Climb to 99000 feet, squawk 7777.")?;
    let mut engine = Engine::new(binary, speech, &model, kind == "test-radio")?;
    match kind {
        "test-radio" => radio::radio(&mut engine, &model)?,
        "test-crew" => crew::crew(&mut engine, &model)?,
        "test-speech" => speech::speech_examples(&engine, &model)?,
        _ => return Err("Unknown integration suite".into()),
    };
    println!("{} passed against the real engine", kind);
    Ok(())
}
