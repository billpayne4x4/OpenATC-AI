#include "openatc/core.hpp"
#include <cctype>
#include <cstdio>
#include <fstream>
#include <regex>
#include <sstream>
#include <stdexcept>
#include <filesystem>
#include <limits>
#include <map>
#include <queue>
#include <set>

namespace openatc {
const std::vector<RequestDefinition>& requestDefinitions() {
    static const std::vector<RequestDefinition> definitions={
        {"clearance","Request IFR clearance","Ground"},{"pushback","Request pushback","Ground"},{"taxi","Request taxi","Ground"},{"ready","Ready for departure","Ground"},{"gate","Taxi to parking","Ground"},{"progressive","Repeat taxi instructions","Ground"},
        {"radio_check","Radio check","Communication"},{"readback","Read back clearance","Communication"},{"repeat","Say again","Communication"},{"standby","Stand by","Communication"},{"unable","Unable","Communication"},{"frequency","Request frequency change","Communication"},{"position","Confirm position","Information"},
        {"altitude","Request altitude...","Enroute",true},{"direct","Request direct-to...","Enroute",true},{"descent","Request descent...","Enroute",true},{"cancel_ifr","Cancel IFR","Enroute"},{"approach","Brief arrival approach","Arrival"},{"go_around","Going around","Arrival"},{"emergency","Declare emergency","Emergency"}
    }; return definitions;
}
std::string phaseName(Phase phase) { static const char* names[]={"Parked","Clearance","Taxi","Departure","Cruise","Descent","Approach","Landed","Pushback","Taxi to parking","Finished"}; int index=static_cast<int>(phase); return index>=0 && index<11?names[index]:"Unknown"; }
namespace {
std::string plural(long value,const char* one,const char* many){return std::to_string(value)+" "+(value==1||value==-1?one:many);}
}
UnitSystem resolveUnits(const std::string& preference,const std::string& departureIcao) {
    if(preference=="metric")return UnitSystem::Metric;
    if(preference=="region"&&!departureIcao.empty()){char prefix=static_cast<char>(std::toupper(static_cast<unsigned char>(departureIcao.front())));if(prefix=='K')return UnitSystem::Imperial;if(prefix=='Z'||prefix=='U')return UnitSystem::Metric;return UnitSystem::Hybrid;}
    return UnitSystem::Imperial;
}
double feetToMeters(double feet){return feet*0.3048;}
int metersToFeet(int meters){return static_cast<int>(std::lround(meters/0.3048/100)*100);}
std::string altitudeText(double feet,UnitSystem units,bool speech) {
    if(units!=UnitSystem::Metric){long whole=static_cast<long>(std::lround(feet));if(speech)return plural(whole,"foot","feet");return std::to_string(whole)+" ft";}
    long meters=std::lround(feet*0.3048/10)*10;
    if(speech)return plural(meters,"meter","meters");
    return std::to_string(meters)+" m";
}
std::string speedText(double knots,UnitSystem units,bool speech) {
    if(units!=UnitSystem::Metric){long whole=static_cast<long>(std::lround(knots));if(speech)return plural(whole,"knot","knots");return std::to_string(whole)+" kt";}
    long kmh=std::lround(knots*1.852);
    if(speech)return plural(kmh,"kilometer per hour","kilometers per hour");
    return std::to_string(kmh)+" km/h";
}
std::string distanceText(double nm,UnitSystem units,bool speech) {
    if(units!=UnitSystem::Metric){long whole=static_cast<long>(std::lround(nm));if(speech)return plural(whole,"mile","miles");return std::to_string(whole)+" NM";}
    long km=std::lround(nm*1.852);
    if(speech)return plural(km,"kilometer","kilometers");
    return std::to_string(km)+" km";
}
std::string climbRateText(double fpm,UnitSystem units,bool speech) {
    if(units!=UnitSystem::Metric){long whole=static_cast<long>(std::lround(fpm));if(speech)return plural(whole,"foot per minute","feet per minute");return std::to_string(whole)+" fpm";}
    double ms=std::lround(fpm*0.00508*10)/10.0;
    char text[32];std::snprintf(text,sizeof(text),"%.1f",ms);
    if(speech)return std::string(text)+(ms==1||ms==-1?" meter per second":" meters per second");
    return std::string(text)+" m/s";
}
UnitSystem unitsForRegion(const Region& region) {
    if(region.altitude=="meters")return UnitSystem::Metric;
    if(region.pressure=="altimeter")return UnitSystem::Imperial;
    return UnitSystem::Hybrid;
}
Region builtinRegion(const std::string& icao) {
    Region region;
    if(icao.empty())return region;
    char prefix=static_cast<char>(std::toupper(static_cast<unsigned char>(icao.front())));
    if(prefix=='K'){region.name="us";region.pressure="altimeter";region.clearance="initial";region.transitionFeet=18000;return region;}
    if(prefix=='Z'){region.name="china";region.altitude="meters";region.transitionFeet=20000;return region;}
    if(prefix=='U'){region.name="russia";region.altitude="meters";region.transitionFeet=15000;return region;}
    if(prefix=='Y'){region.name="australia";return region;}
    if(prefix=='E'||prefix=='L'){region.name="europe";return region;}
    return region;
}
std::map<std::string,Region> loadRegions(const std::string& path) {
    std::ifstream input(path);
    if(!input)throw std::runtime_error("Cannot open regions file: "+path);
    std::map<std::string,Region> table;std::string section;std::map<std::string,std::string> keys;std::string line;
    auto commit=[&]{if(section.empty())return;Region region;region.name=section;auto get=[&](const std::string& key){auto found=keys.find(key);if(found==keys.end())throw std::runtime_error("Region "+section+" misses "+key);return found->second;};std::string prefixes=get("prefixes");region.pressure=get("pressure");region.altitude=get("altitude");region.clearance=get("clearance");int transitionFeet=std::stoi(get("transition_feet"));if(prefixes.size()<2||prefixes.front()!='['||prefixes.back()!=']')throw std::runtime_error("Region "+section+" prefixes must be a list");if(region.pressure!="qnh"&&region.pressure!="altimeter")throw std::runtime_error("Region "+section+" pressure must be qnh or altimeter");if(region.altitude!="feet"&&region.altitude!="meters")throw std::runtime_error("Region "+section+" altitude must be feet or meters");if(region.clearance!="initial"&&region.clearance!="sid")throw std::runtime_error("Region "+section+" clearance must be initial or sid");if(transitionFeet<=0)throw std::runtime_error("Region "+section+" transition must be positive");region.transitionFeet=transitionFeet;std::string inner=prefixes.substr(1,prefixes.size()-2);std::istringstream list(inner);std::string prefix;while(std::getline(list,prefix,',')){size_t first=prefix.find_first_not_of(" \t\""),last=prefix.find_last_not_of(" \t\"");if(first==std::string::npos)continue;prefix=prefix.substr(first,last-first+1);if(prefix.size()!=1)throw std::runtime_error("Region "+section+" prefixes hold single letters");table[prefix]=region;}keys.clear();};
    while(std::getline(input,line)) {
        size_t first=line.find_first_not_of(" \t\r\n");if(first==std::string::npos||line[first]=='#')continue;size_t last=line.find_last_not_of(" \t\r\n");line=line.substr(first,last-first+1);
        if(line.front()=='['&&line.back()==']'){commit();std::string name=line.substr(1,line.size()-2);const std::string head="regions.";if(name.compare(0,head.size(),head)!=0)throw std::runtime_error("Regions file holds only [regions.NAME] sections");section=name.substr(head.size());continue;}
        size_t equals=line.find('=');if(equals==std::string::npos||section.empty())throw std::runtime_error("Malformed regions line: "+line);std::string key=line.substr(0,equals),value=line.substr(equals+1);size_t keyFirst=key.find_first_not_of(" \t"),keyLast=key.find_last_not_of(" \t");size_t valueFirst=value.find_first_not_of(" \t"),valueLast=value.find_last_not_of(" \t");if(keyFirst==std::string::npos||valueFirst==std::string::npos)throw std::runtime_error("Malformed regions line: "+line);key=key.substr(keyFirst,keyLast-keyFirst+1);value=value.substr(valueFirst,valueLast-valueFirst+1);if(value.size()>=2&&value.front()=='"'&&value.back()=='"')value=value.substr(1,value.size()-2);if(key!="prefixes"&&key!="pressure"&&key!="altitude"&&key!="clearance"&&key!="transition_feet")throw std::runtime_error("Unknown regions key: "+key);keys[key]=value;
    }
    commit();
    return table;
}
Region regionFor(const std::map<std::string,Region>& table,const std::string& icao) {
    if(!icao.empty()){std::string prefix(1,static_cast<char>(std::toupper(static_cast<unsigned char>(icao.front()))));auto found=table.find(prefix);if(found!=table.end())return found->second;}
    for(const auto& entry:table)if(entry.second.name=="icao")return entry.second;
    return builtinRegion(icao);
}
std::string pressureText(const Weather& weather,const Region& region,bool speech) {
    std::optional<int> qnh=weather.qnh, altimeter=weather.altimeter;
    if(!qnh&&altimeter)qnh=static_cast<int>(std::lround(*altimeter/2.953));
    if(!altimeter&&qnh)altimeter=static_cast<int>(std::lround(*qnh*2.953));
    if(region.pressure=="altimeter"&&altimeter){char text[16];std::snprintf(text,sizeof(text),"%d.%02d",*altimeter/100,*altimeter%100);if(speech)return "Altimeter "+std::to_string(*altimeter);return std::string(text)+" inHg";}
    if(qnh){if(speech)return "QNH "+std::to_string(*qnh);return "QNH "+std::to_string(*qnh);}
    return speech?"pressure unknown":"pressure --";
}
std::string regionNotes(const Region& region,UnitSystem units) {
    std::string altitude=units==UnitSystem::Metric?"meters":"feet";
    return "Local procedure ("+region.name+"): "+(region.pressure=="altimeter"?"altimeter in inches of mercury":"QNH in hectopascals")+"; altitudes in "+altitude+"; transition altitude "+std::to_string(region.transitionFeet)+" feet.";
}
namespace {
std::string reportText(const Json& report,const char* key){if(!report.is_object()||!report.contains(key)||!report[key].is_string())return {};return report[key].get<std::string>();}
}
std::vector<WeatherHazard> evaluateWeather(const Json& reports,const std::string& departure,const std::string& destination,const std::string& alternate,Phase phase) {
    std::vector<WeatherHazard> hazards;
    if(!reports.is_array())return hazards;
    for(const auto& report:reports) {
        if(!report.is_object())continue;
        std::string station=reportText(report,"icaoId");
        if(station.empty())continue;
        bool watched=station==departure||station==destination||station==alternate;
        if(!watched)continue;
        bool terminal=station==destination||station==alternate;
        std::string present=reportText(report,"wxString"),raw=reportText(report,"rawOb"),category=reportText(report,"fltCat");
        if(present.find("TS")!=std::string::npos)hazards.push_back({station,"thunderstorm","thunderstorm"});
        else if(present.find("GR")!=std::string::npos)hazards.push_back({station,"hail","hail"});
        else if(present.find("FZ")!=std::string::npos)hazards.push_back({station,"freezing_rain","freezing rain"});
        else if(present.find("VA")!=std::string::npos)hazards.push_back({station,"volcanic_ash","volcanic ash"});
        if(phase==Phase::Approach&&std::regex_search(raw,std::regex(R"(\bWS\b)")))hazards.push_back({station,"windshear","windshear"});
        if(terminal&&(category=="IFR"||category=="LIFR"))hazards.push_back({station,"ifr",category+" conditions"});
    }
    return hazards;
}
bool advisoryKnown(const State& state,const std::string& station,const std::string& hazard) {
    for(const auto& advisory:state.weatherAdvisories)if(advisory.station==station&&advisory.hazard==hazard)return true;
    return false;
}
void rememberAdvisory(State& state,const std::string& station,const std::string& hazard,double observed) {
    if(advisoryKnown(state,station,hazard))return;
    state.weatherAdvisories.push_back({station,hazard,observed});
}
void clearAdvisory(State& state,const std::string& station,const std::string& hazard) {
    state.weatherAdvisories.erase(std::remove_if(state.weatherAdvisories.begin(),state.weatherAdvisories.end(),[&](const WeatherAdvisory& advisory){return advisory.station==station&&advisory.hazard==hazard;}),state.weatherAdvisories.end());
}
std::string reportWord(const Json& report,const char* key) {if(!report.is_object()||!report.contains(key)||!report[key].is_string())return {};return report[key].get<std::string>();}
double reportNumber(const Json& report,const char* key) {if(!report.is_object()||!report.contains(key)||report[key].is_null())return 0;if(report[key].is_number())return report[key].get<double>();return 0;}
std::string advisoryText(const std::string& callsign,const Json& report,const WeatherHazard& hazard,const Region& region,UnitSystem units) {
    std::string station=hazard.station;
    auto windWord=[&]{double direction=reportNumber(report,"wdir"),speed=reportNumber(report,"wspd"),gust=reportNumber(report,"wgst");if(speed<=0)return std::string("wind calm");char wind[64];if(reportWord(report,"wdir")=="VRB"||direction<=0)std::snprintf(wind,sizeof(wind),"variable at %.0f",speed);else std::snprintf(wind,sizeof(wind),"%.0f at %.0f",direction,speed);std::string text=wind;if(gust>speed){char gusts[32];std::snprintf(gusts,sizeof(gusts)," gusting %.0f knots",gust);text+=gusts;}else text+=" knots";return "wind "+text;};
    auto visibilityWord=[&]{double visib=reportNumber(report,"visib");if(units==UnitSystem::Metric){long meters=std::lround(visib*1609/100)*100;return plural(meters,"meter","meters");}char text[32];std::snprintf(text,sizeof(text),"%.2g",visib);return std::string(text)+" miles";};
    std::string pressure=pressureText(parseMetar(reportWord(report,"rawOb")),region,true);
    if(hazard.kind=="windshear")return callsign+", windshear reported at "+station+". Advise intentions.";
    if(hazard.kind=="ifr"){std::string ceiling;if(report.contains("clouds")&&report["clouds"].is_array()){double lowest=0;std::string cover;for(const auto& layer:report["clouds"]){std::string kind=layer.contains("cover")&&layer["cover"].is_string()?layer["cover"].get<std::string>():std::string{};double base=layer.contains("base")&&layer["base"].is_number()?layer["base"].get<double>():0;if((kind=="BKN"||kind=="OVC")&&(lowest==0||base<lowest)){lowest=base;cover=kind=="OVC"?"overcast":"broken";}}if(lowest>0)ceiling=", ceiling "+altitudeText(lowest,units,true)+" "+cover;}return callsign+", "+station+" is now "+hazard.summary+ceiling+", "+pressure+". Advise intentions.";}
    return callsign+", "+station+" weather: "+hazard.summary+", "+windWord()+", visibility "+visibilityWord()+", "+pressure+". Advise intentions.";
}
void addTransmission(State& state,const std::string& speaker,const std::string& text,const SpeechTag& tag) {
    Transmission entry;entry.speaker=speaker;entry.text=text;entry.sequence=state.nextSequence++;entry.position=tag.position;entry.voice=tag.voice;entry.speed=tag.speed;entry.delivery=tag.delivery;entry.urgent=tag.urgent;
    state.transcript.push_back(std::move(entry));
    if(state.transcript.size()>500) state.transcript.erase(state.transcript.begin());
}
std::string controllerService(const State& state) {
    return state.phase==Phase::Parked?"Clearance":state.telemetry.onGround?(state.phase==Phase::Taxi||state.phase==Phase::Departure?"Tower":"Ground"):(state.phase==Phase::Departure?"Departure":"Approach");
}
std::string controllerAirspace(const State& state,const Airport* airport) {
    std::string icao;
    if(airport && !airport->icao.empty())icao=airport->icao;
    else {bool arrival=state.phase==Phase::Arrival||state.phase==Phase::Approach||state.phase==Phase::Landed||state.phase==Phase::TaxiIn;icao=arrival?state.plan.destination:state.plan.departure;}
    return icao+":"+controllerService(state);
}
std::vector<std::string> parseVoicePool(const std::string& pool) {
    std::vector<std::string> voices;std::string token;std::istringstream input(pool);
    while(std::getline(input,token,',')) {
        size_t first=token.find_first_not_of(" \t\r\n"),last=token.find_last_not_of(" \t\r\n");
        if(first==std::string::npos)continue;voices.push_back(token.substr(first,last-first+1));
    }
    return voices;
}
DeliveryPreset deliveryPreset(const std::string& name) {
    if(name=="brisk")return {1.1f,0.12f,0.05f};
    if(name=="urgent")return {1.22f,0.06f,0.02f};
    return {1.0f,0.25f,0.1f};
}
Controller assignController(Controllers& roster,const std::string& key,const std::vector<std::string>& pool,const std::string& fallbackVoice,const std::string& delivery,float speedMin,float speedMax,std::mt19937& rng) {
    auto found=roster.assignments.find(key);
    if(found!=roster.assignments.end())return found->second;
    std::vector<std::string> candidates=pool.empty()?std::vector<std::string>{fallbackVoice}:pool;
    size_t exclude=candidates.size()>1?std::min<size_t>(4,candidates.size()-1):0;
    std::set<std::string> excluded;
    for(size_t index=0;index<exclude&&index<roster.recent.size();++index)excluded.insert(roster.recent[roster.recent.size()-1-index]);
    std::vector<std::string> fresh;
    for(const auto& voice:candidates)if(!excluded.count(voice))fresh.push_back(voice);
    if(fresh.empty())fresh=candidates;
    if(speedMax<speedMin)std::swap(speedMin,speedMax);
    std::uniform_int_distribution<size_t> pick(0,fresh.size()-1);
    std::uniform_real_distribution<float> pace(speedMin,speedMax);
    Controller controller{fresh[pick(rng)],delivery,pace(rng)};
    roster.assignments[key]=controller;roster.recent.push_back(controller.voice);
    while(roster.recent.size()>8)roster.recent.erase(roster.recent.begin());
    return controller;
}
Request interpretText(const std::string& text) {
    std::string normalized=text;
    std::transform(normalized.begin(),normalized.end(),normalized.begin(),[](unsigned char value){return static_cast<char>(std::tolower(value));});
    Request request; request.text=text; request.intent="conversation";
    if(normalized=="radio check") request.intent="radio_check";
    else if(normalized=="say again" || normalized=="repeat") request.intent="repeat";
    else if(normalized=="unable") request.intent="unable";
    else if(normalized=="stand by") request.intent="standby";
    else if(normalized=="request clearance" || normalized=="request ifr clearance") request.intent="clearance";
    else if(normalized=="request taxi") request.intent="taxi";
    else if(normalized=="request pushback") request.intent="pushback";
    else if(normalized=="ready for departure") request.intent="ready";
    else if(normalized=="going around" || normalized=="go around") request.intent="go_around";
    else if(normalized=="cancel ifr") request.intent="cancel_ifr";
    else if(normalized=="request altimeter") request.intent="altimeter";
    else if(normalized=="request weather") request.intent="weather";
    if(normalized.find("mayday")!=std::string::npos||normalized.find("pan pan")!=std::string::npos||normalized=="emergency"||normalized=="declare emergency") request.intent="emergency";
    std::smatch match;
    if(std::regex_match(normalized,match,std::regex(R"((?:request )?(?:altitude |climb |descend |descent )(?:to )?(fl\s*)?([0-9]{2,5})(?: feet| meters)?)"))) {
        request.intent=normalized.find("desc")!=std::string::npos?"descent":"altitude";
        request.altitudeFeet=std::stoi(match[2].str())*(match[1].matched?100:1);
        if(normalized.find("meter")!=std::string::npos&&!match[1].matched)request.altitudeFeet=metersToFeet(request.altitudeFeet);
    }
    if(std::regex_match(normalized,match,std::regex(R"((?:request )?direct(?: to)? ([a-z0-9]{2,10}))"))) {
        request.intent="direct"; request.waypoint=match[1];
        std::transform(request.waypoint.begin(),request.waypoint.end(),request.waypoint.begin(),[](unsigned char c){return static_cast<char>(std::toupper(c));});
    }
    if(request.intent=="conversation")for(const auto& definition:requestDefinitions()){std::string title=definition.title;std::transform(title.begin(),title.end(),title.begin(),[](unsigned char value){return std::tolower(value);});if(title==normalized){request.intent=definition.intent;break;}}
    return request;
}
Result applyRequest(State& state,const Request& request,const Airport* airport,const SpeechTag& atcTag,const SpeechTag& pilotTag,const Realism& realism,UnitSystem units,const Region& region) {
    addTransmission(state,state.plan.callsign,request.text.empty()?request.intent:request.text,pilotTag);
    auto reply=[&](bool accepted,std::string message){SpeechTag tag=atcTag;if(request.intent=="go_around"||request.intent=="emergency"){tag.urgent=true;tag.delivery="urgent";}addTransmission(state,"ATC",message,tag); return Result{accepted,std::move(message)};};
    static const std::set<std::string> openIntents={"radio_check","repeat","standby","unable","frequency","checkin"};
    bool gated=openIntents.count(request.intent)==0;
    if(gated&&realism.requireFrequency&&state.frequencySequence>0&&state.telemetry.com1Khz>0&&state.telemetry.com1Khz!=state.recommendedFrequencyKhz) {
        std::string contact="Contact the controller";if(airport)for(const auto& frequency:airport->frequencies)if(frequency.khz==state.recommendedFrequencyKhz){contact="Contact "+frequency.name;break;}
        char megahertz[16];std::snprintf(megahertz,sizeof(megahertz),"%.3f",state.recommendedFrequencyKhz/1000.0);
        return reply(false,contact+" on "+megahertz+" MHz."+(realism.teachingCorrections?" Tune COM1 to the assigned frequency before requesting service.":""));
    }
    if(gated&&realism.requireCallsign&&!state.plan.callsign.empty()) {
        std::string text=request.text,call=state.plan.callsign,title;
        std::transform(text.begin(),text.end(),text.begin(),[](unsigned char c){return static_cast<char>(std::tolower(c));});
        std::transform(call.begin(),call.end(),call.begin(),[](unsigned char c){return static_cast<char>(std::tolower(c));});
        bool button=false;for(const auto& definition:requestDefinitions()){title=definition.title;std::transform(title.begin(),title.end(),title.begin(),[](unsigned char c){return static_cast<char>(std::tolower(c));});if(title==text){button=true;break;}}
        if(!button&&text.find(call)==std::string::npos)return reply(false,std::string("Say callsign.")+(realism.teachingCorrections?" Include your callsign so the controller knows who is calling.":""));
    }
    if(request.intent=="radio_check") return reply(true,"Reading you five. Development controller online.");
    if(request.intent=="repeat") { for(auto entry=state.transcript.rbegin();entry!=state.transcript.rend();++entry) if(entry->speaker=="ATC") {auto message=entry->text; return reply(true,message);} return reply(false,"No previous controller transmission."); }
    if(request.intent=="standby") return reply(true,"Standing by.");
    if(request.intent=="unable") return reply(true,"Roger unable. Existing clearance remains recorded; request an alternative.");
    if(request.intent=="readback") {
        if(!state.clearance || state.clearance->acknowledged) return reply(false,"No pending clearance to acknowledge.");
        if(request.clearanceSequence!=state.clearance->sequence || request.altitudeFeet!=state.clearance->altitudeFeet || request.waypoint!=state.clearance->route) {
            if(!realism.strictReadbacks&&request.clearanceSequence==state.clearance->sequence){state.clearance->acknowledged=true;return reply(true,"Readback correct for recorded altitude and route.");}
            return reply(false,"Readback mismatch or stale clearance. Check altitude and route.");
        }
        state.clearance->acknowledged=true; return reply(true,"Readback correct for recorded altitude and route.");
    }
    if(!requestAvailable(state,request.intent)) return reply(false,"That request is not available during "+phaseName(state.phase)+".");
    const std::string prefix=state.demo?"DEMO: ":"";
    if(request.intent=="clearance") {
        if(!state.telemetry.onGround || state.phase!=Phase::Parked) return reply(false,"Clearance is available only while parked.");
        state.clearance=Clearance{state.plan.initialAltitudeFeet,state.plan.route,state.plan.runway,"2105",false,state.nextSequence}; state.phase=Phase::Clearance;
        std::string altitude=altitudeText(state.plan.initialAltitudeFeet,units,true);
        if(region.clearance=="initial")return reply(true,prefix+"Cleared to "+state.plan.destination+" via "+(state.plan.sid.empty()?state.plan.route:state.plan.sid+" then "+state.plan.route)+", initial altitude "+altitude+", squawk 2105. Departure runway "+state.plan.runway+".");
        std::string via=state.plan.sid.empty()?state.plan.route:state.plan.sid+" then "+state.plan.route;
        std::string climb=state.plan.sid.empty()?"climb to "+altitude:"climb via "+state.plan.sid+" to "+altitude;
        return reply(true,prefix+"Cleared to "+state.plan.destination+" via "+via+", "+climb+", squawk 2105. Departure runway "+state.plan.runway+".");
    }
    if(request.intent=="pushback") {
        if(!state.telemetry.onGround) return reply(false,"Pushback unavailable airborne.");
        state.phase=Phase::Pushback; return reply(true,prefix+"Pushback approved. Advise ready to taxi.");
    }
    if(request.intent=="taxi" || request.intent=="gate") {
        if(!airport) return reply(false,"Load this airport's taxi network before requesting taxi.");
        if(!state.demo && airport->icao!=(request.intent=="gate"?state.plan.destination:state.plan.departure)) return reply(false,"The loaded surface airport does not match this flight.");
        try {
            Telemetry position=state.telemetry;
            if(state.demo && !position.positionValid && !airport->parking.empty()) {
                position.latitude=airport->referenceLatitude+airport->parking.front().point.north/111320.0;
                position.longitude=airport->referenceLongitude+airport->parking.front().point.east/(111320.0*std::cos(airport->referenceLatitude*3.141592653589793/180));
                position.positionValid=true;
            }
            auto route=calculateTaxiRoute(*airport,position,request.intent=="gate"?state.plan.arrivalStand:state.plan.runway,request.intent=="gate");
            route.approved=true;route.sequence=state.nextSequence;state.taxiClearance=route;
            state.phase=request.intent=="gate"?Phase::TaxiIn:Phase::Taxi;
            return reply(true,prefix+route.instructions);
        } catch(const std::exception& error) {return reply(false,error.what());}
    }
    if(request.intent=="ready") {
        if(!state.taxiClearance.approved || state.taxiClearance.points.empty())return reply(false,"A taxi clearance is required before departure.");
        if(!state.demo && airport) {
            auto position=airportPoint(*airport,state.telemetry.latitude,state.telemetry.longitude);
            auto end=state.taxiClearance.points.back();
            if(std::hypot(position.east-end.east,position.north-end.north)>180)return reply(false,"Continue to the end of the approved taxi route before reporting ready.");
        }
        state.phase=Phase::Departure;state.taxiClearance.approved=false;
        return reply(true,prefix+"Runway "+state.plan.runway+", cleared for takeoff. OpenATC AI does not manage other traffic.");
    }
    if(request.intent=="altitude" || request.intent=="descent" || request.intent=="direct") {
        if(state.telemetry.onGround || !state.clearance) return reply(false,"An airborne flight with an existing clearance is required.");
        Clearance proposed=*state.clearance;
        if(request.intent=="direct") {if(!std::regex_match(request.waypoint,std::regex("[A-Z0-9]{2,10}"))) return reply(false,"Enter a waypoint identifier."); if(!state.demo && std::none_of(state.plan.fixes.begin(),state.plan.fixes.end(),[&](const RouteFix& fix){return fix.identifier==request.waypoint;}))return reply(false,"That fix is not in the imported route. Import a georeferenced plan before requesting direct-to.");proposed.route=request.waypoint;}
        else {if(request.altitudeFeet<1000 || request.altitudeFeet>45000 || request.altitudeFeet%100!=0) return reply(false,"Enter an altitude from "+altitudeText(1000,units,true)+" to "+altitudeText(45000,units,true)+(units==UnitSystem::Metric?", in 10-meter steps.":", in 100-foot increments.")); proposed.altitudeFeet=request.altitudeFeet;}
        if(request.intent=="descent"){if(request.altitudeFeet>=state.telemetry.altitudeFeet)return reply(false,"Descent altitude must be below your current altitude.");state.phase=Phase::Arrival;}
        proposed.acknowledged=false; proposed.sequence=state.nextSequence; state.clearance=proposed;
        return reply(true,prefix+"Maintain "+altitudeText(proposed.altitudeFeet,units,true)+", route "+proposed.route+".");
    }
    if(request.intent=="cancel_ifr") {state.ifr=false;return reply(true,prefix+"IFR cancelled.");}
    if(request.intent=="go_around") {state.phase=Phase::Departure;state.phaseEvidence=0;return reply(true,prefix+"Go-around recorded. Follow your published missed approach.");}
    if(request.intent=="progressive") return reply(true,state.taxiClearance.instructions);
    if(request.intent=="frequency" || request.intent=="checkin") {
        if(!airport)return reply(false,"Load the airport frequencies first.");
        if(state.phase==Phase::Cruise)return reply(false,"Enroute sector frequencies are not loaded. No frequency assigned.");
        std::string expectedAirport=(state.phase==Phase::Arrival||state.phase==Phase::Approach||state.phase==Phase::Landed||state.phase==Phase::TaxiIn)?state.plan.destination:state.plan.departure;
        if(!state.demo&&airport->icao!=expectedAirport)return reply(false,"Load "+expectedAirport+" before requesting its frequency.");
        std::string service=state.phase==Phase::Parked?"Clearance":state.telemetry.onGround?(state.phase==Phase::Taxi||state.phase==Phase::Departure?"Tower":"Ground"):(state.phase==Phase::Departure?"Departure":"Approach");
        for(const auto& frequency:airport->frequencies)if(frequency.service==service){state.recommendedFrequencyKhz=frequency.khz;state.frequencySequence=state.nextSequence;return reply(true,"Contact "+frequency.name+" on "+std::to_string(frequency.khz/1000.0)+" MHz.");}
        return reply(false,"No "+service+" frequency is present in the loaded airport data.");
    }
    if(request.intent=="approach") {state.phase=Phase::Approach;return reply(true,"Approach briefing: "+state.plan.star+" "+state.plan.approach+", runway "+state.plan.arrivalRunway+". No vectors or procedure clearance issued.");}
    if(request.intent=="position"){if(!state.telemetry.positionValid)return reply(false,"Aircraft position unavailable.");return reply(true,"Position "+std::to_string(state.telemetry.latitude)+", "+std::to_string(state.telemetry.longitude)+".");}
    if(request.intent=="emergency") {
        if(!realism.practiceEmergencies)return reply(false,"Emergency practice is off. Enable practice emergencies in Realism settings.");
        return reply(true,"Roger mayday. Squawk 7700, state intentions and souls on board. Priority handling.");
    }
    if(!realism.strictPhraseology)return reply(false,"This request is in the catalogue but its operational procedure is not implemented yet.");
    return reply(false,std::string("Say again with a standard request.")+(realism.teachingCorrections?" For example: 'request taxi' or 'request altitude FL320'.":""));
}
double descentDistanceNm(double altitudeFeet,double targetFeet,double angleDegrees) {
    if(!std::isfinite(altitudeFeet)||!std::isfinite(targetFeet)||!std::isfinite(angleDegrees)||angleDegrees<=0||angleDegrees>=15) throw std::invalid_argument("Invalid descent profile input");
    return std::max(0.0,altitudeFeet-targetFeet)/(6076.11549*std::tan(angleDegrees*3.141592653589793/180.0));
}
Airport demoAirport() {
    Airport airport; airport.icao="DEMO"; airport.name="Coastal International"; airport.source="Synthetic geometry - not a real airport";
    airport.runways.push_back({"09","27",{-1200,0,0},{1200,0,0},45});
    airport.nodes={{1,{-1000,200,0}},{2,{0,200,0}},{3,{1000,200,0}},{4,{-1000,0,0}},{5,{1000,0,0}},{6,{0,430,0}}};
    airport.edges={{1,2,"A",false,false,"",'F'},{2,3,"A",false,false,"",'F'},{1,4,"A1",false,false,"",'F'},{3,5,"A2",false,false,"",'F'},{2,6,"B",false,false,"",'F'}};
    airport.parking={{"Gate 1","gate","jets","C","airline",{0,430,0},180},{"Gate 2","gate","jets","C","airline",{250,430,0},180},{"GA apron","misc","props","A","general_aviation",{-400,400,0},90}};
    airport.frequencies={{"Ground","Demo Ground",121900},{"Tower","Demo Tower",118100},{"Departure","Demo Departure",123800},{"Approach","Demo Approach",125500}};
    return airport;
}
Airport loadAirport(const std::string& path,const std::string& icao) {
    std::ifstream input(path); if(!input) throw std::runtime_error("Cannot open apt.dat");
    Airport airport; airport.source=path; bool selected=false, referenceSet=false, modernFrequencies=false; std::string line;
    auto project=[&](double latitude,double longitude){if(!referenceSet){airport.referenceLatitude=latitude;airport.referenceLongitude=longitude;referenceSet=true;} return Point{(longitude-airport.referenceLongitude)*111320.0*std::cos(airport.referenceLatitude*3.141592653589793/180.0),(latitude-airport.referenceLatitude)*111320.0,0};};
    while(std::getline(input,line)) {
        std::istringstream row(line); int record=0; if(!(row>>record)) continue;
        if(record==1 || record==16 || record==17) {if(selected) break; double elevation; int tower,unused; std::string identifier; if(!(row>>elevation>>tower>>unused>>identifier)) continue; selected=identifier==icao; if(selected){airport.icao=identifier;airport.elevationFeet=elevation;std::getline(row,airport.name);}continue;}
        if(!selected) continue;
        if(record==100) {
            double width,shoulderSmooth,latitude1,longitude1,latitude2,longitude2,displaced1,blast1,displaced2,blast2; int surface,shoulder,centerLights,edgeLights,signs,mark1,approach1,touch1,reil1,mark2,approach2,touch2,reil2; std::string first,second;
            if(row>>width>>surface>>shoulder>>shoulderSmooth>>centerLights>>edgeLights>>signs>>first>>latitude1>>longitude1>>displaced1>>blast1>>mark1>>approach1>>touch1>>reil1>>second>>latitude2>>longitude2>>displaced2>>blast2>>mark2>>approach2>>touch2>>reil2) airport.runways.push_back({first,second,project(latitude1,longitude1),project(latitude2,longitude2),width});
        } else if(record==1201) {double latitude,longitude;std::string usage;long identifier;if(row>>latitude>>longitude>>usage>>identifier)airport.nodes.push_back({identifier,project(latitude,longitude)});}
        else if(record==1202) {long first,second;std::string direction,usage,name;if(row>>first>>second>>direction>>usage){std::getline(row,name);name.erase(0,name.find_first_not_of(" \t"));TaxiEdge edge{first,second,name,usage=="runway",direction=="oneway","",'F'};if(usage.rfind("taxiway_",0)==0 && usage.size()>8)edge.size=usage.back();airport.edges.push_back(edge);}}
        else if(record==1204 && !airport.edges.empty()) {std::string operation,runways;row>>operation;std::getline(row,runways);airport.edges.back().activeRunways+=runways;}
        else if(record==1300 || record==15) {double latitude,longitude,heading;std::string type="misc",equipment="unknown",name;if(row>>latitude>>longitude>>heading){if(record==1300)row>>type>>equipment;std::getline(row,name);name.erase(0,name.find_first_not_of(" \t"));airport.parking.push_back({name,type,equipment,"","",project(latitude,longitude),heading});}}
        else if(record==1301 && !airport.parking.empty()) {row>>airport.parking.back().size>>airport.parking.back().operations;}
        else if((record>=50 && record<=56)||(record>=1050 && record<=1056)) {if(record>=1050 && !modernFrequencies){airport.frequencies.clear();modernFrequencies=true;}if(record<1050 && modernFrequencies)continue;int frequency;std::string name;if(row>>frequency){std::getline(row,name);name.erase(0,name.find_first_not_of(" \t"));static const char* services[]={"ATIS","Unicom","Clearance","Ground","Tower","Approach","Departure"};airport.frequencies.push_back({services[record%1000-50],name,record>=1000?frequency:frequency*10});}}
    }
    if(airport.icao.empty())throw std::runtime_error("Airport identifier not found in apt.dat");
    if(airport.runways.empty())throw std::runtime_error("Airport has no supported land runways");
    std::sort(airport.frequencies.begin(),airport.frequencies.end(),[](const Frequency& first,const Frequency& second){return std::tie(first.service,first.khz)<std::tie(second.service,second.khz);});
    airport.frequencies.erase(std::unique(airport.frequencies.begin(),airport.frequencies.end(),[](const Frequency& first,const Frequency& second){return first.service==second.service && first.khz==second.khz;}),airport.frequencies.end());
    return airport;
}
Weather parseMetar(const std::string& raw) {
    Weather weather;weather.raw=raw;std::smatch match;
    if(std::regex_search(raw,match,std::regex(R"(\bQ([0-9]{4})\b)")))weather.qnh=std::stoi(match[1]);
    if(std::regex_search(raw,match,std::regex(R"(\bA([0-9]{4})\b)"))){weather.altimeter=std::stoi(match[1]);if(!weather.qnh)weather.qnh=static_cast<int>(std::lround(*weather.altimeter*0.3386389));}
    if(std::regex_search(raw,match,std::regex(R"(\b(?:[0-9]{3}|VRB)[0-9]{2,3}(?:G[0-9]{2,3})?KT\b)")))weather.wind=match[0];
    if(raw.find("CAVOK")!=std::string::npos){weather.visibility="10 km or more";weather.clouds="CAVOK";}
    else {if(std::regex_search(raw,match,std::regex(R"(\b[0-9]{1,2}(?:/[0-9])?SM\b|\b9999\b)")))weather.visibility=match[0]; if(std::regex_search(raw,match,std::regex(R"(\b(?:FEW|SCT|BKN|OVC)[0-9]{3}(?:CB|TCU)?\b)")))weather.clouds=match[0];}
    return weather;
}
}
