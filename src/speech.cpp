#include "openatc/speech.hpp"
#include "openatc/serialization.hpp"
#include <httplib.h>
#define MINIAUDIO_IMPLEMENTATION
#include <miniaudio.h>
#include <atomic>
#include <future>
#include <mutex>
#include <thread>
#include <vector>
#include <cstring>
#include <cmath>

namespace openatc {
struct Speech::Implementation {
    ma_context context{};ma_device capture{},playback{};ma_decoder decoder{};
    bool contextReady=false,captureReady=false,playbackReady=false,decoderReady=false,recording=false,monitorOnly=false;
    std::vector<ma_device_info> inputs,outputs;std::string inputKey,outputKey;
    std::atomic<float> rms{0},peak{0},gain{1};std::atomic<bool> full{false},playing{false};float masterVolume=0.8f,pendingVolume=1;
    std::atomic<int> callbackDepth{0};
    std::mutex audioMutex;std::vector<int16_t> samples;std::string audioBytes,transcript,status="Microphone off";std::future<std::pair<bool,std::string>> pending;
    static std::string key(const ma_device_info& info,size_t index){return std::string(info.name)+" #"+std::to_string(index);}
    const ma_device_id* findDevice(const std::vector<ma_device_info>& devices,const std::string& requested){if(requested.empty())return nullptr;for(size_t index=0;index<devices.size();++index)if(key(devices[index],index)==requested)return &devices[index].id;throw std::runtime_error("Selected audio device is unavailable. Refresh devices and select it again.");}
    void stopPlayback(){playing=false;if(playbackReady)ma_device_stop(&playback);
        int spins=0;while(callbackDepth.load()>0&&spins<10000){std::this_thread::yield();++spins;}
        if(playbackReady){ma_device_uninit(&playback);playbackReady=false;}if(decoderReady){ma_decoder_uninit(&decoder);decoderReady=false;}audioBytes.clear();}
    void stopCapture(){if(captureReady)ma_device_stop(&capture);recording=false;monitorOnly=false;rms=0;peak=0;}
    ~Implementation(){if(captureReady)ma_device_uninit(&capture);stopPlayback();if(contextReady)ma_context_uninit(&context);}
};
Speech::Speech():implementation_(std::make_unique<Implementation>()){}Speech::~Speech()=default;
bool Speech::recording()const{return implementation_->recording && !implementation_->monitorOnly;}
bool Speech::monitoring()const{return implementation_->recording && implementation_->monitorOnly;}
float Speech::inputDb()const{return 20*std::log10(std::max(implementation_->rms.load(),0.000001f));}
float Speech::inputLevel()const{return std::clamp((inputDb()+60)/60,0.0f,1.0f);}
bool Speech::clipping()const{return implementation_->peak>=0.99f;}
bool Speech::busy()const{return implementation_->pending.valid()||implementation_->playing;}
std::string Speech::status()const{return implementation_->status;}
std::vector<AudioDevice> Speech::inputDevices()const{std::vector<AudioDevice> devices={{"","System default"}};auto& audio=*implementation_;for(size_t index=0;index<audio.inputs.size();++index)devices.push_back({Implementation::key(audio.inputs[index],index),audio.inputs[index].name});return devices;}
std::vector<AudioDevice> Speech::outputDevices()const{std::vector<AudioDevice> devices={{"","System default"}};auto& audio=*implementation_;for(size_t index=0;index<audio.outputs.size();++index)devices.push_back({Implementation::key(audio.outputs[index],index),audio.outputs[index].name});return devices;}
void Speech::refreshDevices(){auto& audio=*implementation_;if(audio.recording||busy())return;try{if(!audio.contextReady){if(ma_context_init(nullptr,0,nullptr,&audio.context)!=MA_SUCCESS)throw std::runtime_error("Audio initialization failed");audio.contextReady=true;}ma_device_info* inputs=nullptr;ma_device_info* outputs=nullptr;ma_uint32 inputCount=0,outputCount=0;if(ma_context_get_devices(&audio.context,&outputs,&outputCount,&inputs,&inputCount)!=MA_SUCCESS)throw std::runtime_error("Cannot enumerate audio devices");audio.inputs.clear();audio.outputs.clear();for(ma_uint32 index=0;index<inputCount;++index)audio.inputs.push_back(inputs[index]);for(ma_uint32 index=0;index<outputCount;++index)audio.outputs.push_back(outputs[index]);audio.status="Audio devices refreshed";}catch(const std::exception& error){audio.status=error.what();}}
void Speech::configure(const std::string& input,const std::string& output,float gain,float volume){auto& audio=*implementation_;audio.gain=std::clamp(gain,0.0f,4.0f);audio.masterVolume=std::clamp(volume,0.0f,1.0f);if(audio.playbackReady)ma_device_set_master_volume(&audio.playback,audio.masterVolume*audio.pendingVolume);if(input!=audio.inputKey && !audio.recording){if(audio.captureReady){ma_device_uninit(&audio.capture);audio.captureReady=false;}audio.inputKey=input;}if(!audio.playing)audio.outputKey=output;}
void Speech::startRecording(bool monitorOnly){auto& audio=*implementation_;if(busy()||audio.recording)return;if(!audio.contextReady)refreshDevices();try{if(!audio.contextReady)throw std::runtime_error("No audio context");audio.stopPlayback();if(!audio.captureReady){auto configuration=ma_device_config_init(ma_device_type_capture);configuration.capture.pDeviceID=audio.findDevice(audio.inputs,audio.inputKey);configuration.capture.format=ma_format_s16;configuration.capture.channels=1;configuration.sampleRate=16000;configuration.pUserData=&audio;
    configuration.dataCallback=[](ma_device* device,void*,const void* input,ma_uint32 count){auto& data=*static_cast<Implementation*>(device->pUserData);if(!input||count==0)return;auto* samples=static_cast<const int16_t*>(input);double sum=0;float peak=0,gain=data.gain;std::lock_guard<std::mutex> guard(data.audioMutex);for(ma_uint32 index=0;index<count;++index){float sample=std::clamp(samples[index]/32768.0f*gain,-1.0f,1.0f);sum+=sample*sample;peak=std::max(peak,std::abs(sample));if(!data.monitorOnly && data.samples.size()<480000)data.samples.push_back(static_cast<int16_t>(sample*32767));}float rms=static_cast<float>(std::sqrt(sum/count));data.rms=std::max(rms,data.rms.load()*0.85f);data.peak=peak;if(!data.monitorOnly&&data.samples.size()>=480000)data.full=true;};
    if(ma_device_init(&audio.context,&configuration,&audio.capture)!=MA_SUCCESS)throw std::runtime_error("Cannot open selected microphone");audio.captureReady=true;}
    {std::lock_guard<std::mutex> guard(audio.audioMutex);audio.samples.clear();audio.samples.reserve(480000);}audio.monitorOnly=monitorOnly;audio.full=false;audio.recording=true;if(ma_device_start(&audio.capture)!=MA_SUCCESS){audio.recording=false;throw std::runtime_error("Cannot start microphone");}audio.status=monitorOnly?"Microphone test (not sent)":"Recording (maximum 30 seconds)";
    }catch(const std::exception& error){audio.status=error.what();}}
void Speech::stopMonitoring(){auto& audio=*implementation_;audio.stopCapture();audio.status="Microphone off";}
void Speech::transcribe(const std::string& engineEndpoint){auto& audio=*implementation_;if(!audio.recording||audio.monitorOnly||busy())return;audio.stopCapture();std::string wave;auto append=[&](uint32_t value,int bytes){for(int index=0;index<bytes;++index)wave.push_back(static_cast<char>((value>>(8*index))&255));};
    {std::lock_guard<std::mutex> guard(audio.audioMutex);uint32_t bytes=static_cast<uint32_t>(audio.samples.size()*2);if(bytes<3200){audio.status="Recording too short";return;}wave="RIFF";append(36+bytes,4);wave+="WAVEfmt ";append(16,4);append(1,2);append(1,2);append(16000,4);append(32000,4);append(2,2);append(16,2);wave+="data";append(bytes,4);for(auto sample:audio.samples)append(static_cast<uint16_t>(sample),2);}
    audio.status="Transcribing...";audio.pending=std::async(std::launch::async,[engineEndpoint,wave=std::move(wave)](){httplib::Client client(engineEndpoint);client.set_connection_timeout(2);client.set_read_timeout(35);client.set_write_timeout(10);auto response=client.Post("/speech/transcribe",wave,"audio/wav");if(!response||response->status!=200)throw std::runtime_error("Transcription failed. Check engine STT settings.");return std::make_pair(false,Json::parse(response->body).at("text").get<std::string>());});
}
void Speech::speak(const std::string& text,const std::string& engineEndpoint,int role,float volume,const std::string& voice,float speed,bool urgent,const std::string& delivery){auto& audio=*implementation_;if(busy()||audio.recording)return;audio.pendingVolume=std::clamp(volume,0.0f,1.0f);audio.status="Generating speech...";audio.pending=std::async(std::launch::async,[text,engineEndpoint,role,voice,speed,urgent,delivery](){httplib::Client client(engineEndpoint);client.set_connection_timeout(2);client.set_read_timeout(35);client.set_write_timeout(5);Json body={{"text",text},{"speaker",role==4?"ground":(role==3?"cabin":(role==2?"pilot":(role==1?"copilot":"atc")))},{"delivery",delivery},{"urgent",urgent}};if(!voice.empty())body["voice"]=voice;if(speed>0)body["speed"]=speed;auto response=client.Post("/speech/speak",body.dump(),"application/json");if(!response||response->status!=200)throw std::runtime_error("Speech synthesis failed. Check engine TTS settings.");return std::make_pair(true,response->body);});}
void Speech::poll(){auto& audio=*implementation_;if(audio.full && audio.recording){audio.status="30 seconds recorded. Select Transcribe to send.";}
    if(audio.playbackReady && !audio.playing){audio.stopPlayback();audio.status="Speech complete";}
    if(!audio.pending.valid()||audio.pending.wait_for(std::chrono::seconds(0))!=std::future_status::ready)return;
    try{auto result=audio.pending.get();if(!result.first){audio.transcript=result.second;audio.status="Transcript ready - review and transmit";return;}if(!audio.contextReady)refreshDevices();audio.stopPlayback();audio.audioBytes=std::move(result.second);auto decoderConfig=ma_decoder_config_init(ma_format_f32,2,48000);if(ma_decoder_init_memory(audio.audioBytes.data(),audio.audioBytes.size(),&decoderConfig,&audio.decoder)!=MA_SUCCESS)throw std::runtime_error("TTS response is not decodable audio");audio.decoderReady=true;auto configuration=ma_device_config_init(ma_device_type_playback);configuration.playback.pDeviceID=audio.findDevice(audio.outputs,audio.outputKey);configuration.playback.format=ma_format_f32;configuration.playback.channels=2;configuration.sampleRate=48000;configuration.pUserData=&audio;
    configuration.dataCallback=[](ma_device* device,void* output,const void*,ma_uint32 frames){auto& audio=*static_cast<Implementation*>(device->pUserData);++audio.callbackDepth;ma_uint64 read=0;ma_decoder_read_pcm_frames(&audio.decoder,output,frames,&read);if(read<frames){std::memset(static_cast<float*>(output)+read*2,0,static_cast<size_t>(frames-read)*2*sizeof(float));audio.playing=false;}--audio.callbackDepth;};
    if(ma_device_init(&audio.context,&configuration,&audio.playback)!=MA_SUCCESS)throw std::runtime_error("Cannot open selected output device");audio.playbackReady=true;ma_device_set_master_volume(&audio.playback,audio.masterVolume*audio.pendingVolume);audio.playing=true;if(ma_device_start(&audio.playback)!=MA_SUCCESS)throw std::runtime_error("Cannot play speech");audio.status="Playing speech";
    }catch(const std::exception& error){audio.stopPlayback();audio.status=error.what();}}
std::string Speech::takeTranscript(){auto result=std::move(implementation_->transcript);implementation_->transcript.clear();return result;}
}
