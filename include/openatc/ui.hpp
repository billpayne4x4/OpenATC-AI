#pragma once
#include "client.hpp"
#include "speech.hpp"
#include <imgui.h>
#include <deque>
namespace openatc {
double uiTimeSeconds();
bool editText(const char* label,std::string& value,size_t capacity=256,ImGuiInputTextFlags flags=0);
bool editParagraph(const char* label,std::string& value,float height=80,size_t capacity=4096);
void sectionTitle(const char* title);
void drawSymbol(ImDrawList* draw,int symbol,ImVec2 center,ImU32 color,float radius=10);
class Interface {
    Speech speech_;EngineClient& engine_;int page_=0;bool settingsLoaded_=false,planLoaded_=false;
    Settings settings_;FlightPlan draft_;Airport airport_=demoAirport();
    std::string message_,search_,requestedWaypoint_,airportCode_="YMLT",weatherStations_,notice_,planNotice_,airportNotice_,weatherNotice_,settingsNotice_;
    Json weatherReports_=Json::array();std::string lastSettingsJson_;double settingsChangedAt_=0,lastInteraction_=0,lastWeatherFetch_=0;
    std::string modalIntent_,modalTitle_;int requestedAltitude_=32000;float targetFeet_=3000,descentAngle_=3;
    bool showVoicePopup_=false;int commRole_=0;
    float zoom_=1;ImVec2 pan_{};float weatherZoom_=1;ImVec2 weatherPan_{};
    bool importing_=false,loadingAirport_=false,loadingWeather_=false,showRunways_=true,showNavaids_=true,orbitView_=false,showBuildings_=true;float yaw_=0.35f,pitch_=0.85f;
    unsigned lastTranscriptSequence_=0,lastSpeechSequence_=0;std::deque<Transmission> speechQueue_;
    void pollReplies();void drawRequestDialog(const State& state);void updateSpeech(const State& state);
    void atcPage(const State&);void planPage(const State&);void taxiPage(const State&);void cruisePage(const State&);void arrivalPage(const State&);void airportsPage(const State&);void weatherPage(const State&);void settingsPage(const State&);
    void airportCanvas(const State& state,float height=0);void weatherCanvas(const State& state,float height);void send(Request request);void metric(const char* title,const std::string& value,float width=180);
    void loadAirport(const std::string& icao);void saveSettings();void speakEntry(const Transmission& entry);void showVoicePopup();
public:
    explicit Interface(EngineClient& engine):engine_(engine){}
    static void configureStyle();static void configureFonts();
    void tick();void draw(ImVec2 position,ImVec2 size);
    void setPage(int page){page_=std::clamp(page,0,7);}
};
}
