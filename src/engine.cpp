#include "openatc/branding.hpp"
#include "openatc/simbrief.hpp"
#include <httplib.h>
#include <algorithm>
#include <cctype>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <mutex>
#include <regex>
#include <thread>
#include <vector>

using namespace openatc;
namespace {
std::filesystem::path configurationPath() {
    if(const char* path=std::getenv("OPENATC_CONFIG_DIR"))return std::filesystem::path(path);
#ifdef _WIN32
    if(const char* path=std::getenv("APPDATA"))return std::filesystem::path(path)/"OpenATC";
#else
    if(const char* path=std::getenv("XDG_CONFIG_HOME"))return std::filesystem::path(path)/"openatc";
    if(const char* path=std::getenv("HOME"))return std::filesystem::path(path)/".config/openatc";
#endif
    return "openatc-config";
}
void saveJson(const std::filesystem::path& path,const Json& data) {
    std::filesystem::create_directories(path.parent_path());auto temporary=path;temporary+=".tmp";
    {std::ofstream output(temporary,std::ios::trunc);output<<data.dump(2);output.flush();if(!output)throw std::runtime_error("Cannot save configuration");}
#ifdef _WIN32
    std::error_code ignored;std::filesystem::remove(path,ignored);
#endif
    std::filesystem::rename(temporary,path);
}
void configureClient(httplib::Client& client) {client.set_connection_timeout(5);client.set_read_timeout(25);client.set_write_timeout(10);client.set_follow_location(true);client.enable_server_certificate_verification(true);}
httplib::Headers authorization(const char* name) {httplib::Headers headers;if(const char* key=std::getenv(name))headers.emplace("Authorization",std::string("Bearer ")+key);return headers;}
struct Prompts { std::string cabin, ground, copilot, classify, copilotChat, source="built-in"; };
std::string readPromptFile(const std::filesystem::path& directory,const std::string& name) {std::ifstream input(directory/name);if(!input)return {};std::string text((std::istreambuf_iterator<char>(input)),std::istreambuf_iterator<char>());while(!text.empty()&&(text.back()=='\n'||text.back()=='\r'))text.pop_back();return text;}
std::string fillPrompt(std::string text,const std::string& callsign) {size_t at=0;while((at=text.find("{{callsign}}",at))!=std::string::npos){text.replace(at,12,callsign);at+=callsign.size();}return text;}
std::filesystem::file_time_type fileStamp(const std::filesystem::path& file){std::error_code code;auto stamp=std::filesystem::last_write_time(file,code);return code?std::filesystem::file_time_type::min():stamp;}
std::string resolveRegionsFile(const std::filesystem::path& directory,const std::filesystem::path& executable) {
    if(const char* override=std::getenv("OPENATC_REGIONS_FILE"))return override;
    std::vector<std::string> candidates={(directory/"regions.toml").string(),(executable/"../regions.toml").string()};
#ifdef OPENATC_PROMPTS_SOURCE_DIR
    candidates.push_back((std::filesystem::path(OPENATC_PROMPTS_SOURCE_DIR)/"../regions.toml").string());
#endif
    for(const auto& candidate:candidates){std::error_code code;if(std::filesystem::is_regular_file(candidate,code))return std::filesystem::path(candidate).lexically_normal().string();}
    return {};
}
Prompts loadPrompts(const std::filesystem::path& directory,const std::filesystem::path& executable) {
    Prompts prompts;
    prompts.cabin="You are the cabin crew of flight {{callsign}}, speaking with the captain on the flight-deck interphone. Answer briefly and warmly in one or two sentences. Play along with service requests. For technical or safety questions you cannot answer, say what you will do about it yourself. The captain IS the flight deck: never say you will check with the flight deck or cockpit. Return only the reply text.";
    prompts.ground="You are the ground crew working flight {{callsign}}, speaking with the captain. Answer briefly with ramp discipline in one or two sentences. You handle chocks, ground power and pushback readiness. For anything outside that, say you will confirm and report back. The captain IS the flight deck: never say you will confirm with the flight deck or cockpit. Return only the reply text.";
    prompts.copilot="Read back the clearance in one short ICAO-style transmission, repeating altitude, route and squawk exactly. Return only the readback text.";
    prompts.classify="Classify the pilot message. Return only JSON with intent (radio_check,repeat,standby,unable,clearance,taxi,pushback,ready,go_around,cancel_ifr,altitude,descent,direct,approach,gate,frequency,emergency,conversation), altitudeFeet integer and waypoint string. Never issue clearances. Use conversation if uncertain.";
    prompts.copilotChat="You are the first officer of flight {{callsign}}, speaking with the captain. Answer briefly in one or two sentences. You work the radios when asked and you know the clearance, route and flight stage. Never invent clearances or procedures; say what you will do or what you need from the captain. Return only the reply text.";
    std::vector<std::filesystem::path> candidates;
    if(const char* override=std::getenv("OPENATC_PROMPTS_DIR"))candidates.push_back(override);
    candidates.push_back(directory/"prompts");
    candidates.push_back(executable/"../prompts");
#ifdef OPENATC_PROMPTS_SOURCE_DIR
    candidates.push_back(OPENATC_PROMPTS_SOURCE_DIR);
#endif
    for(const auto& candidate:candidates){if(readPromptFile(candidate,"cabin.txt").empty())continue;prompts.source=candidate.string();std::string text;if(!(text=readPromptFile(candidate,"cabin.txt")).empty())prompts.cabin=text;if(!(text=readPromptFile(candidate,"ground.txt")).empty())prompts.ground=text;if(!(text=readPromptFile(candidate,"copilot_readback.txt")).empty())prompts.copilot=text;if(!(text=readPromptFile(candidate,"copilot_chat.txt")).empty())prompts.copilotChat=text;if(!(text=readPromptFile(candidate,"intent_classify.txt")).empty())prompts.classify=text;break;}
    return prompts;
}
Json fetchMetarReports(const std::string& stations) {
    if(!std::regex_match(stations,std::regex("[A-Z0-9]{4}(,[A-Z0-9]{4}){0,39}")))throw std::invalid_argument("Enter station ICAOs separated by commas.");
    httplib::Client client("https://aviationweather.gov");configureClient(client);
    auto result=client.Get(("/api/data/metar?ids="+stations+"&format=json&hours=2").c_str());
    if(!result||result->status!=200)throw std::runtime_error("Weather download failed or no current reports are available.");
    auto reports=Json::parse(result->body);
    if(!reports.is_array())throw std::runtime_error("Invalid weather response");
    return reports;
}
std::string chatContent(const Json& parsed){auto content=parsed.at("choices").at(0).at("message").at("content");return content.is_string()?content.get<std::string>():std::string{};}
void validateSettings(Settings& settings) {
    for(const auto& url:{settings.aiUrl,settings.sttUrl,settings.ttsUrl})if(!std::regex_match(url,std::regex(R"(https?://[^/\s]+/?$)")))throw std::invalid_argument("Service URLs must be http(s) origins, without /v1 paths.");
    for(float value:{settings.masterVolume,settings.controllerVolume,settings.copilotVolume,settings.pilotVolume,settings.attendantVolume,settings.groundVolume})if(!std::isfinite(value)||value<0||value>1)throw std::invalid_argument("Volume must be between 0 and 1.");
    if(!std::isfinite(settings.inputGain)||settings.inputGain<0||settings.inputGain>4||!std::isfinite(settings.ttsSpeed)||settings.ttsSpeed<0.25||settings.ttsSpeed>4||!std::isfinite(settings.uiScale)||settings.uiScale<0.85||settings.uiScale>1.5)throw std::invalid_argument("Invalid audio or UI scale setting.");
    for(float value:{settings.controllerSpeedMin,settings.controllerSpeedMax,settings.copilotSpeed,settings.pilotSpeed})if(!std::isfinite(value)||value<0.5f||value>2.0f)throw std::invalid_argument("Voice speeds must be between 0.5 and 2.");
    if(settings.controllerSpeedMin>settings.controllerSpeedMax)throw std::invalid_argument("Controller minimum speed must not exceed maximum speed.");
    for(const auto& delivery:{settings.controllerDelivery,settings.copilotDelivery})if(delivery!="standard"&&delivery!="brisk"&&delivery!="urgent")throw std::invalid_argument("Unknown delivery style.");
    if(settings.congestion!="off"&&settings.congestion!="quiet"&&settings.congestion!="busy")throw std::invalid_argument("Unknown congestion level.");
    if(settings.units!="imperial"&&settings.units!="metric"&&settings.units!="region")throw std::invalid_argument("Unknown units mode.");
    for(const auto& ref:{settings.attendantRef,settings.groundRef}){if(ref.size()>256||!std::regex_match(ref,std::regex("[A-Za-z0-9_/]*")))throw std::invalid_argument("Crew datarefs must be dataref names.");}
    if(settings.copilotPersonality.size()>2000||settings.attendantPersonality.size()>2000||settings.groundPersonality.size()>2000)throw std::invalid_argument("Personality text too long.");
    for(const auto& voice:parseVoicePool(settings.voicePool))if(!std::regex_match(voice,std::regex("[A-Za-z0-9_-]+")))throw std::invalid_argument("Voice pool entries must be voice names separated by commas.");
    for(float value:{settings.radioHiss,settings.radioCrackle,settings.radioStatic})if(!std::isfinite(value)||value<0||value>1)throw std::invalid_argument("Radio effect levels must be between 0 and 1.");
    settings.fadeDelay=std::clamp(settings.fadeDelay,1.0f,60.0f);settings.fadedOpacity=std::clamp(settings.fadedOpacity,0.1f,1.0f);
}
}
int main(int argumentCount,char** arguments) {
    int port=argumentCount>1?std::stoi(arguments[1]):8087;
    State state;state.plan.runway="09";Settings settings;Airport airport=demoAirport();std::mutex stateMutex;httplib::Server server;auto directory=configurationPath();
    try{std::ifstream input(directory/"settings.json");if(input){Json saved;input>>saved;settings=saved.get<Settings>();validateSettings(settings);}}catch(const std::exception& error){std::cerr<<"Settings: "<<error.what()<<"\n";settings=Settings{};}
    Controllers controllers;std::mt19937 voiceRng{std::random_device{}()};
    std::filesystem::file_time_type settingsStamp=fileStamp(directory/"settings.json");
    auto saveControllers=[&]{saveJson(directory/"controllers.json",controllers);};
    try{std::ifstream input(directory/"controllers.json");if(input){Json saved;input>>saved;controllers=saved.get<Controllers>();}}catch(const std::exception& error){std::cerr<<"Controllers: "<<error.what()<<"\n";controllers=Controllers{};}
    if(const char* value=std::getenv("OPENATC_AI_URL")){settings.aiUrl=value;settings.aiEnabled=true;}if(const char* value=std::getenv("OPENATC_AI_MODEL"))settings.aiModel=value;
    Prompts prompts=loadPrompts(directory,std::filesystem::path(arguments[0]).parent_path());
    std::map<std::string,Region> regionTable;std::string regionsPath=resolveRegionsFile(directory,std::filesystem::path(arguments[0]).parent_path());
    if(!regionsPath.empty()){try{regionTable=loadRegions(regionsPath);}catch(const std::exception& error){std::cerr<<"Regions: "<<error.what()<<"\n";regionsPath.clear();}}
    server.set_payload_max_length(4*1024*1024);server.set_read_timeout(30,0);server.set_write_timeout(30,0);
    auto writeResult=[](auto& response,const Json& value){response.set_content(value.dump(),"application/json");};
    auto handleError=[&](auto& response,const std::exception& error){response.status=400;writeResult(response,{{"error",error.what()}});};
    server.Get("/health",[&](const auto&,auto& response){writeResult(response,{{"service","open-atc"},{"protocol",2},{"version",productVersion},{"prompts",prompts.source},{"regions",regionsPath.empty()?"built-in":regionsPath}});});
    auto serviceOk=[](const std::string& url){if(url.empty())return false;httplib::Client client(url);client.set_connection_timeout(2);client.set_read_timeout(3);client.set_write_timeout(3);client.set_follow_location(true);auto result=client.Get("/health");return result&&result->status==200;};
    server.Get("/voice-health",[&](const auto&,auto& response){Settings configuration;{std::lock_guard<std::mutex> guard(stateMutex);configuration=settings;}writeResult(response,{{"stt",serviceOk(configuration.sttUrl)},{"tts",serviceOk(configuration.ttsUrl)}});});
    server.Get("/state",[&](const auto&,auto& response){std::lock_guard<std::mutex> guard(stateMutex);Json snapshot=state;snapshot["settings"]=settings;writeResult(response,snapshot);});
    server.Post("/settings",[&](const auto& incoming,auto& response){try{auto proposed=Json::parse(incoming.body).template get<Settings>();validateSettings(proposed);std::lock_guard<std::mutex> guard(stateMutex);saveJson(directory/"settings.json",proposed);settings=proposed;saveControllers();writeResult(response,settings);}catch(const std::exception& error){handleError(response,error);}});
    server.Post("/simulator/root",[&](const auto& incoming,auto& response){try{auto root=Json::parse(incoming.body).at("root").template get<std::string>();if(!std::filesystem::is_directory(root))throw std::runtime_error("Invalid X-Plane folder");std::lock_guard<std::mutex> guard(stateMutex);if(settings.simulatorRoot.empty()){settings.simulatorRoot=root;saveJson(directory/"settings.json",settings);}writeResult(response,{{"accepted",true}});}catch(const std::exception& error){handleError(response,error);}});
    server.Post("/request",[&](const auto& incoming,auto& response){try {
        Request request=Json::parse(incoming.body).template get<Request>();if(request.text.size()>4000)throw std::invalid_argument("Message too long");
        std::string role=request.role;if(role!="cabin"&&role!="ground"&&role!="copilot")role="atc";
        if(request.intent.empty()||request.intent=="conversation")request=interpretText(request.text);
        request.role=role;
        Settings configuration;{std::lock_guard<std::mutex> guard(stateMutex);configuration=settings;}
        if(request.role=="cabin"||request.role=="ground") {
            // Cockpit-exempt: realism discipline grades frequency talk only, never crew chat.
            bool attendant=request.role=="cabin";std::string speaker=attendant?"CABIN":"GROUND";
            std::string voice=attendant?configuration.attendantVoice:configuration.groundVoice;if(voice.empty())voice="alloy";
            std::string callsign;{std::lock_guard<std::mutex> guard(stateMutex);callsign=state.plan.callsign;}
            std::string base=attendant?fillPrompt(prompts.cabin,callsign):fillPrompt(prompts.ground,callsign);
            std::string personality=attendant?configuration.attendantPersonality:configuration.groundPersonality;
            std::string replyText;
            if(!configuration.aiEnabled||configuration.aiModel.empty())replyText="Crew chat needs AI intent classification on.";
            else{try{
                httplib::Client client(configuration.aiUrl);configureClient(client);
                std::string system=personality.empty()?base:(personality+" "+base);
                Json body={{"model",configuration.aiModel},{"temperature",0.7},{"messages",Json::array({{ {"role","system"},{"content",system} },{{"role","user"},{"content","Captain: "+request.text}}})}};
                auto generated=client.Post("/v1/chat/completions",authorization("OPENATC_AI_KEY"),body.dump(),"application/json");
                if(generated&&generated->status==200){auto parsed=Json::parse(generated->body);replyText=chatContent(parsed);
                    size_t first=replyText.find_first_not_of(" \t\r\n\"'"),last=replyText.find_last_not_of(" \t\r\n\"'");
                    replyText=first==std::string::npos?"":replyText.substr(first,last-first+1);
                    if(replyText.size()>500)replyText.resize(500);}
            }catch(...){}
            if(replyText.empty())replyText="Crew line is busy. Try again.";}
            bool ok=replyText!="Crew chat needs AI intent classification on."&&replyText!="Crew line is busy. Try again.";
            std::lock_guard<std::mutex> guard(stateMutex);
            addTransmission(state,state.plan.callsign,request.text.empty()?request.role:request.text,{});
            addTransmission(state,speaker,replyText,{"",voice,"standard",1,false});
            writeResult(response,{{"result",Result{ok,replyText}},{"state",state}});return;
        }
        if(request.intent=="conversation" && configuration.aiEnabled && !configuration.aiModel.empty()) {
            Realism level{configuration.strictReadbacks,configuration.requireFrequency,configuration.requireCallsign,configuration.strictPhraseology,configuration.teachingCorrections,configuration.practiceEmergencies};
            std::string preset=disciplinePreset(level,configuration.congestion);
            std::string classify=prompts.classify+(preset=="relaxed"?" Accept casual phrasing and slang; resolve the best-guess intent.":(preset=="real"?" Require standard phraseology; use conversation when unsure.":""));
            httplib::Client client(configuration.aiUrl);configureClient(client);
            Json body={{"model",configuration.aiModel},{"temperature",0},{"messages",Json::array({{{"role","system"},{"content",classify}},{{"role","user"},{"content",request.text}}})}};
            auto result=client.Post("/v1/chat/completions",authorization("OPENATC_AI_KEY"),body.dump(),"application/json");if(!result||result->status!=200)throw std::runtime_error("AI endpoint unavailable. Use request buttons or supported text commands.");
            auto parsed=Json::parse(result->body);auto contentText=parsed.at("choices").at(0).at("message").at("content");if(contentText.is_string()){auto classification=dropNulls(Json::parse(contentText.get<std::string>()));auto interpreted=classification.template get<Request>();interpreted.text=request.text;interpreted.role=role;request=interpreted;if(request.intent=="readback")request.intent="conversation";}
        }
        request.role=role;
        // Cockpit-exempt: the copilot path below ignores realism like cabin/ground above.
        // Talk addresses the copilot; the copilot never transmits to ATC from here.
        if(request.role=="copilot") {
            std::string callsign;{std::lock_guard<std::mutex> guard(stateMutex);callsign=state.plan.callsign;}
            std::string system=configuration.copilotPersonality.empty()?fillPrompt(prompts.copilotChat,callsign):(configuration.copilotPersonality+" "+fillPrompt(prompts.copilotChat,callsign));
            if(request.intent!="conversation")system+=" The captain addressed an ATC task to you, but you cannot transmit on frequency. Briefly redirect them to say it themselves, then offer to read it back.";
            std::string voice=configuration.copilotVoice;if(voice.empty())voice="alloy";
            std::string replyText;
            if(!configuration.aiEnabled||configuration.aiModel.empty())replyText="Copilot chat needs AI intent classification on.";
            else{try{
                httplib::Client client(configuration.aiUrl);configureClient(client);
                Json body={{"model",configuration.aiModel},{"temperature",0.7},{"messages",Json::array({{ {"role","system"},{"content",system} },{{"role","user"},{"content","Captain: "+request.text}}})}};
                auto generated=client.Post("/v1/chat/completions",authorization("OPENATC_AI_KEY"),body.dump(),"application/json");
                if(generated&&generated->status==200){auto parsed=Json::parse(generated->body);replyText=chatContent(parsed);
                    size_t first=replyText.find_first_not_of(" \t\r\n\"'"),last=replyText.find_last_not_of(" \t\r\n\"'");
                    replyText=first==std::string::npos?"":replyText.substr(first,last-first+1);
                    if(replyText.size()>500)replyText.resize(500);}
            }catch(...){}
            if(replyText.empty())replyText="Copilot line is busy. Try again.";}
            bool ok=replyText!="Copilot chat needs AI intent classification on."&&replyText!="Copilot line is busy. Try again.";
            std::lock_guard<std::mutex> guard(stateMutex);
            addTransmission(state,state.plan.callsign,request.text.empty()?request.role:request.text,{});
            addTransmission(state,"COPILOT",replyText,{"",voice,"standard",1,false});
            writeResult(response,{{"result",Result{ok,replyText}},{"state",state}});return;
        }
        if(configuration.congestion=="quiet"||configuration.congestion=="busy")std::this_thread::sleep_for(std::chrono::milliseconds(configuration.congestion=="busy"?1000+std::rand()%3000:500+std::rand()%1500));
        Realism realism{configuration.strictReadbacks,configuration.requireFrequency,configuration.requireCallsign,configuration.strictPhraseology,configuration.teachingCorrections,configuration.practiceEmergencies};
        std::string departure;{std::lock_guard<std::mutex> lookup(stateMutex);departure=state.plan.departure;}
        Region region=regionFor(regionTable,departure);
        UnitSystem units=configuration.units=="region"?unitsForRegion(region):resolveUnits(configuration.units,departure);
        std::string personalityReadback;std::unique_lock<std::mutex> guard(stateMutex);
        if(configuration.copilotReplies&&state.clearance&&!state.clearance->acknowledged&&configuration.aiEnabled&&!configuration.aiModel.empty()&&!configuration.copilotPersonality.empty()) {
            Clearance pending=*state.clearance;std::string callsign=state.plan.callsign;guard.unlock();
            try{
                httplib::Client client(configuration.aiUrl);configureClient(client);
                std::string facts="Clearance: maintain "+std::to_string(pending.altitudeFeet)+" feet, route "+pending.route+", squawk "+pending.squawk+". Callsign "+callsign+".";
                Json body={{"model",configuration.aiModel},{"temperature",0.7},{"messages",Json::array({{ {"role","system"},{"content",configuration.copilotPersonality+" "+prompts.copilot+" "+regionNotes(region,units)} },{{"role","user"},{"content",facts}}})}};
                auto generated=client.Post("/v1/chat/completions",authorization("OPENATC_AI_KEY"),body.dump(),"application/json");
                if(generated&&generated->status==200){auto parsed=Json::parse(generated->body);personalityReadback=chatContent(parsed);
                    size_t first=personalityReadback.find_first_not_of(" \t\r\n\"'"),last=personalityReadback.find_last_not_of(" \t\r\n\"'");
                    personalityReadback=first==std::string::npos?"":personalityReadback.substr(first,last-first+1);
                    if(personalityReadback.size()>500)personalityReadback.resize(500);}
            }catch(...){}
            guard.lock();
        }
        std::string airspace=controllerAirspace(state,&airport);
        auto pool=parseVoicePool(configuration.voicePool);
        std::string delivery=configuration.controllerDelivery;
        if(configuration.randomizeDelivery){static const char* styles[]={"standard","brisk","urgent"};std::uniform_int_distribution<int> pick(0,2);delivery=styles[pick(voiceRng)];}
        size_t rosterBefore=controllers.assignments.size();
        Controller controller=assignController(controllers,airspace,pool,configuration.voice.empty()?std::string("alloy"):configuration.voice,delivery,configuration.controllerSpeedMin,configuration.controllerSpeedMax,voiceRng);
        if(controllers.assignments.size()!=rosterBefore)saveControllers();
        size_t split=airspace.find(':');
        SpeechTag atcTag{split==std::string::npos?airspace:airspace.substr(split+1),controller.voice,controller.delivery,controller.speed,false};
        SpeechTag pilotTag{"",configuration.pilotVoice,"standard",configuration.pilotSpeed,false};
        auto result=applyRequest(state,request,&airport,atcTag,pilotTag,realism,units,region);
        if(configuration.congestion=="busy"){std::uniform_int_distribution<int> roll(0,99);if(roll(voiceRng)<25)addTransmission(state,"ATC","Standby.",atcTag);}
        if(configuration.congestion=="quiet"||configuration.congestion=="busy") {
            static const std::pair<const char*,const char*> chatter[]={
                {"QFA456 request taxi","Qantas 456, taxi approved."},
                {"JST789 ready for departure","Jetstar 789, hold short, traffic on final."},
                {"VOZ123 request altitude FL350","Velocity 123, maintain flight level 350."},
                {"RXA321 going around","Rex 321, roger go-around."},
                {"FDX88 on the gate, request pushback","FedEx 88, pushback approved."}};
            std::uniform_int_distribution<int> roll(0,99);
            int chance=configuration.congestion=="busy"?35:15;
            if(roll(voiceRng)<chance) {
                std::uniform_int_distribution<size_t> pick(0,4);auto exchange=chatter[pick(voiceRng)];
                std::string other=exchange.first,own=state.plan.callsign;
                std::transform(other.begin(),other.end(),other.begin(),[](unsigned char c){return static_cast<char>(std::tolower(c));});
                std::transform(own.begin(),own.end(),own.begin(),[](unsigned char c){return static_cast<char>(std::tolower(c));});
                if(other.compare(0,own.size(),own)!=0&&own.compare(0,3,other,0,3)!=0) {
                    auto pool=parseVoicePool(configuration.voicePool);std::string voice=configuration.copilotVoice;
                    if(!pool.empty()){std::uniform_int_distribution<size_t> draw(0,pool.size()-1);voice=pool[draw(voiceRng)];}
                    if(voice.empty())voice="alloy";
                    std::string caller=exchange.first;size_t space=caller.find(' ');if(space!=std::string::npos)caller.resize(space);
                    addTransmission(state,caller,exchange.first,{"",voice,"standard",1,false});
                    addTransmission(state,"ATC",exchange.second,atcTag);
                }
            }
        }
        if(result.accepted && settings.copilotReplies && state.clearance && !state.clearance->acknowledged){Request readback;readback.intent="readback";readback.altitudeFeet=state.clearance->altitudeFeet;readback.waypoint=state.clearance->route;readback.clearanceSequence=state.clearance->sequence;readback.text=personalityReadback.empty()?("Maintaining "+altitudeText(readback.altitudeFeet,units,true)+", route "+readback.waypoint+", squawk "+state.clearance->squawk+", "+state.plan.callsign+"."):personalityReadback;SpeechTag copilotTag{atcTag.position,configuration.copilotVoice,configuration.copilotDelivery,configuration.copilotSpeed,false};applyRequest(state,readback,&airport,atcTag,copilotTag,Realism{},units,region);if(state.transcript.size()>=2)state.transcript[state.transcript.size()-2].speaker="COPILOT";}
        else if(result.accepted && settings.copilotReplies && (request.intent=="taxi"||request.intent=="gate"||request.intent=="pushback"||request.intent=="frequency")){SpeechTag copilotTag{atcTag.position,configuration.copilotVoice,configuration.copilotDelivery,configuration.copilotSpeed,false};addTransmission(state,"COPILOT",result.message+" "+state.plan.callsign+".",copilotTag);}
        writeResult(response,{{"result",result},{"state",state}});
    }catch(const std::exception& error){handleError(response,error);}});
    server.Post("/plan",[&](const auto& incoming,auto& response){try{auto plan=Json::parse(incoming.body).template get<FlightPlan>();validateFlightPlan(plan);std::lock_guard<std::mutex> guard(stateMutex);if(state.phase!=Phase::Parked && state.phase!=Phase::Finished)throw std::invalid_argument("Finish or reset this flight before replacing its plan.");auto telemetry=state.telemetry;bool demo=state.demo;state=State{};state.plan=plan;state.telemetry=telemetry;state.demo=demo;writeResult(response,state);}catch(const std::exception& error){handleError(response,error);}});
    server.Post("/plan/parking",[&](const auto& incoming,auto& response){try{std::string stand=Json::parse(incoming.body).at("stand").template get<std::string>();std::lock_guard<std::mutex> guard(stateMutex);state.plan.arrivalStand=stand;writeResult(response,{{"accepted",true}});}catch(const std::exception& error){handleError(response,error);}});
    server.Post("/simbrief",[&](const auto& incoming,auto& response){try{std::string identifier=Json::parse(incoming.body).value("userid",std::string{});if(identifier.empty()){std::lock_guard<std::mutex> guard(stateMutex);identifier=settings.simbriefId;}if(!std::regex_match(identifier,std::regex("[0-9]{1,12}")))throw std::invalid_argument("Enter your numeric SimBrief Pilot ID in Settings.");httplib::Client client("https://www.simbrief.com");configureClient(client);auto result=client.Get(("/api/xml.fetcher.php?userid="+identifier+"&json=1").c_str());if(!result||result->status!=200)throw std::runtime_error("SimBrief download failed. Check Pilot ID and generate a flight first.");writeResult(response,parseSimBrief(Json::parse(result->body)));}catch(const std::exception& error){handleError(response,error);}});
    server.Post("/airport/load",[&](const auto& incoming,auto& response){try{auto request=Json::parse(incoming.body);auto icao=request.at("icao").template get<std::string>();if(!std::regex_match(icao,std::regex("[A-Z0-9]{4}")))throw std::invalid_argument("Enter a four-character ICAO.");std::string root;{std::lock_guard<std::mutex> guard(stateMutex);root=settings.simulatorRoot;}Airport loaded=request.value("demo",false)?demoAirport():loadAirportFromSimulator(root,icao);{std::lock_guard<std::mutex> guard(stateMutex);if(airport.icao!=loaded.icao)state.taxiClearance=TaxiClearance{};airport=loaded;}writeResult(response,loaded);}catch(const std::exception& error){handleError(response,error);}});
    server.Post("/weather",[&](const auto& incoming,auto& response){try{auto request=Json::parse(incoming.body);writeResult(response,fetchMetarReports(request.value("stations",std::string{})));}catch(const std::exception& error){handleError(response,error);}});
    server.Post("/speech/transcribe",[&](const auto& incoming,auto& response){try{Settings configuration;{std::lock_guard<std::mutex> guard(stateMutex);configuration=settings;}httplib::Client client(configuration.sttUrl);configureClient(client);httplib::MultipartFormDataItems parts={{"file",incoming.body,"request.wav","audio/wav"},{"model",configuration.sttModel,"",""}};auto result=client.Post("/v1/audio/transcriptions",authorization("OPENATC_STT_KEY"),parts);if(!result||result->status!=200)throw std::runtime_error("STT request failed. Check endpoint, model and OPENATC_STT_KEY on the engine.");auto transcription=dropNulls(Json::parse(result->body));if(!transcription.value("text",Json()).is_string())transcription["text"]="";writeResult(response,transcription);}catch(const std::exception& error){handleError(response,error);}});
    server.Post("/speech/speak",[&](const auto& incoming,auto& response){try{auto request=Json::parse(incoming.body);std::string text=request.at("text").template get<std::string>();if(text.size()>4000)throw std::invalid_argument("Speech text too long");std::string speaker=request.value("speaker",std::string{});
        if(speaker.empty())speaker=request.value("copilot",false)?std::string("copilot"):std::string("atc");
        if(speaker!="atc"&&speaker!="copilot"&&speaker!="pilot"&&speaker!="cabin"&&speaker!="ground")throw std::invalid_argument("Unknown speaker");
        Settings configuration;{std::lock_guard<std::mutex> guard(stateMutex);configuration=settings;}httplib::Client client(configuration.ttsUrl);configureClient(client);
        std::string voice=request.value("voice",std::string{});
        if(voice.empty())voice=speaker=="copilot"?configuration.copilotVoice:(speaker=="pilot"?configuration.pilotVoice:(speaker=="cabin"?configuration.attendantVoice:(speaker=="ground"?configuration.groundVoice:configuration.voice)));
        if(voice.empty())voice="alloy";
        bool urgent=request.value("urgent",false);
        std::string delivery=request.value("delivery",std::string{});
        if(delivery.empty())delivery=speaker=="copilot"?configuration.copilotDelivery:(speaker=="pilot"||speaker=="cabin"||speaker=="ground"?"standard":configuration.controllerDelivery);
        auto preset=deliveryPreset(delivery);
        float speed=request.value("speed",0.0f);
        if(!(speed>0))speed=speaker=="copilot"?configuration.copilotSpeed:(speaker=="pilot"?configuration.pilotSpeed:(speaker=="cabin"||speaker=="ground"?1.0f:(configuration.controllerSpeedMin+configuration.controllerSpeedMax)/2));
        if(urgent){speed=1.22f;preset.sentencePause=0.06f;preset.clausePause=0.02f;}
        speed=std::clamp(configuration.ttsSpeed*speed,0.5f,2.0f);
        bool effectsOn=configuration.radioEffects&&(speaker!="cabin"||configuration.radioFxCabin)&&(speaker!="ground"||configuration.radioFxGround);
        Json body={{"model",configuration.ttsModel},{"voice",voice},{"input",text},{"response_format","wav"},{"speed",speed},{"sentence_pause",preset.sentencePause},{"clause_pause",preset.clausePause},{"effects",{{"hiss",effectsOn?configuration.radioHiss:0},{"crackle",effectsOn?configuration.radioCrackle:0},{"static",effectsOn?configuration.radioStatic:0},{"bandpass",effectsOn&&configuration.radioBandpass}}}};
        auto result=client.Post("/v1/audio/speech",authorization("OPENATC_TTS_KEY"),body.dump(),"application/json");if(!result||result->status!=200)throw std::runtime_error("TTS request failed. Check endpoint, model and OPENATC_TTS_KEY on the engine.");response.set_content(result->body,"audio/wav");}catch(const std::exception& error){handleError(response,error);}});
    server.Post("/telemetry",[&](const auto& incoming,auto& response){try{auto telemetry=Json::parse(incoming.body).template get<Telemetry>();std::lock_guard<std::mutex> guard(stateMutex);updateFlightPhase(state,telemetry);state.demo=false;writeResult(response,{{"accepted",true}});}catch(const std::exception& error){handleError(response,error);}});
    server.Post("/demo",[&](const auto& incoming,auto& response){try{auto update=Json::parse(incoming.body);std::lock_guard<std::mutex> guard(stateMutex);if(!state.demo)throw std::invalid_argument("Demo controls are unavailable while connected to X-Plane.");int phase=update.value("phase",0);if(phase<0||phase>10)throw std::invalid_argument("Invalid phase");state.telemetry=update.at("telemetry").template get<Telemetry>();state.phase=static_cast<Phase>(phase);state.hasDeparted=!state.telemetry.onGround;writeResult(response,state);}catch(const std::exception& error){handleError(response,error);}});
    server.Post("/session/reset",[&](const auto&,auto& response){std::lock_guard<std::mutex> guard(stateMutex);auto telemetry=state.telemetry;bool demo=state.demo;state=State{};state.telemetry=telemetry;state.demo=demo;if(demo)state.plan.runway="09";writeResult(response,state);});
    server.Post("/session/save",[&](const auto&,auto& response){try{std::lock_guard<std::mutex> guard(stateMutex);saveJson(directory/"session.json",state);writeResult(response,{{"saved",true}});}catch(const std::exception& error){handleError(response,error);}});
    server.Post("/session/load",[&](const auto&,auto& response){try{std::ifstream input(directory/"session.json");if(!input)throw std::runtime_error("No saved session");Json saved;input>>saved;State restored=saved.get<State>();std::lock_guard<std::mutex> guard(stateMutex);if(!state.demo)throw std::runtime_error("Cannot restore over a connected simulator flight");state=restored;state.taxiClearance=TaxiClearance{};writeResult(response,state);}catch(const std::exception& error){handleError(response,error);}});
    std::cout<<std::unitbuf;
    std::cout<<productName<<" "<<productVersion<<" engine: http://127.0.0.1:"<<port<<"\n";
    std::thread([&]{
        std::string lastPlan;std::time_t lastFetch=0;
        for(;;) {
            std::this_thread::sleep_for(std::chrono::seconds(30));
            if(fileStamp(directory/"settings.json")!=settingsStamp){try{std::ifstream input(directory/"settings.json");if(input){Json saved;input>>saved;Settings fresh=saved.get<Settings>();validateSettings(fresh);{std::lock_guard<std::mutex> guard(stateMutex);settings=fresh;}std::cerr<<"Settings reloaded from disk\n";}settingsStamp=fileStamp(directory/"settings.json");}catch(const std::exception& error){std::cerr<<"Settings reload: "<<error.what()<<"\n";settingsStamp=fileStamp(directory/"settings.json");}}
            std::string callsign, departure, destination, alternate, unitsPreference;Phase phase=Phase::Parked;
            {std::lock_guard<std::mutex> guard(stateMutex);callsign=state.plan.callsign;departure=state.plan.departure;destination=state.plan.destination;alternate=state.plan.alternate;unitsPreference=settings.units;phase=state.phase;}
            if(departure.empty()&&destination.empty())continue;
            std::string signature=callsign+"/"+departure+"/"+destination+"/"+alternate;
            std::time_t now=std::time(nullptr);
            if(signature==lastPlan&&now-lastFetch<600)continue;
            try {
                std::string stations=departure+(destination.empty()?"":","+destination)+(alternate.empty()?"":","+alternate);
                Json reports=fetchMetarReports(stations);
                Region region=regionFor(regionTable,departure);
                UnitSystem units=unitsPreference=="region"?unitsForRegion(region):resolveUnits(unitsPreference,departure);
                auto hazards=evaluateWeather(reports,departure,destination,alternate,phase);
                std::lock_guard<std::mutex> guard(stateMutex);
                for(const auto& hazard:hazards) {
                    if(advisoryKnown(state,hazard.station,hazard.kind))continue;
                    const Json* match=nullptr;for(const auto& report:reports)if(report.is_object()&&reportWord(report,"icaoId")==hazard.station){match=&report;break;}
                    if(!match)continue;
                    rememberAdvisory(state,hazard.station,hazard.kind,static_cast<double>(now));
                    SpeechTag tag{controllerService(state),settings.voice.empty()?std::string("alloy"):settings.voice,settings.controllerDelivery,(settings.controllerSpeedMin+settings.controllerSpeedMax)/2,hazard.kind!="ifr"};
                    if(tag.delivery.empty())tag.delivery="standard";
                    addTransmission(state,"ATC",advisoryText(callsign,*match,hazard,region,units),tag);
                }
                for(auto known=state.weatherAdvisories.begin();known!=state.weatherAdvisories.end();) {
                    bool current=false;for(const auto& hazard:hazards)if(hazard.station==known->station&&hazard.kind==known->hazard){current=true;break;}
                    if(current)++known;else known=state.weatherAdvisories.erase(known);
                }
                lastPlan=signature;lastFetch=now;
            } catch(...) {lastFetch=now;}
        }
    }).detach();
    if(!server.listen("127.0.0.1",port)){std::cerr<<"Cannot bind engine port\n";return 1;}
}
