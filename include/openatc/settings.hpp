#pragma once
#include <string>
namespace openatc {
struct Settings {
    std::string simbriefId, simulatorRoot;
    std::string aiUrl="http://127.0.0.1:11434", aiModel, sttUrl="http://127.0.0.1:8000", sttModel="whisper-1", ttsUrl="http://127.0.0.1:8001", ttsModel="tts-1", voice="alloy", copilotVoice="am_echo";
    std::string voicePool, controllerDelivery="brisk", copilotDelivery="brisk", pilotVoice="am_adam", copilotPersonality, congestion="quiet", units="imperial";
    std::string attendantVoice="af_sky", groundVoice="bm_george", attendantPersonality, groundPersonality;
    std::string attendantRef, groundRef;
    bool copilotButton=true;
    bool devMode=true;
    float controllerSpeedMin=0.9f, controllerSpeedMax=1.15f, copilotSpeed=1.0f, pilotSpeed=1.0f, pilotVolume=0.8f;
    float radioHiss=0.12f, radioCrackle=0.08f, radioStatic=0.08f, attendantVolume=0.8f, groundVolume=0.8f;
    std::string inputDevice, outputDevice;
    bool aiEnabled=false, copilotReplies=false, copilotTunes=false, controllerSpeech=false, copilotSpeech=false, pilotSpeech=false;
    bool randomizeDelivery=false, radioEffects=true, radioBandpass=true, radioFxCabin=false, radioFxGround=false;
    bool strictReadbacks=true, requireFrequency=false, requireCallsign=false, strictPhraseology=true, teachingCorrections=true, practiceEmergencies=false;
    bool pinOpen=true, showTaxiways=true, showParking=true, showTaxiRoute=true, showOwnship=true, showLabels=true, showPlannedDescent=true, showCurrentDescent=true;
    float masterVolume=0.8f, controllerVolume=1, copilotVolume=0.8f, inputGain=1, ttsSpeed=1, fadeDelay=8, fadedOpacity=0.4f, uiScale=1;
};
}
