#pragma once
#include <memory>
#include <string>
#include <vector>
namespace openatc {
struct AudioDevice {std::string key,name;};
class Speech {
    struct Implementation;std::unique_ptr<Implementation> implementation_;
public:
    Speech();~Speech();
    bool recording()const;bool monitoring()const;bool busy()const;float inputLevel()const;float inputDb()const;bool clipping()const;
    std::vector<AudioDevice> inputDevices()const;std::vector<AudioDevice> outputDevices()const;
    void refreshDevices();void configure(const std::string& input,const std::string& output,float gain,float volume);
    void startRecording(bool monitorOnly=false);void stopMonitoring();void transcribe(const std::string& engineEndpoint);
    void speak(const std::string& text,const std::string& engineEndpoint,int role=0,float volume=1,const std::string& voice={},float speed=0,bool urgent=false,const std::string& delivery="standard");
    void poll();std::string takeTranscript();std::string status()const;
};
}
