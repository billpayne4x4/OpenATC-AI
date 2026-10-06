#include "openatc/core.hpp"
#include "openatc/serialization.hpp"
#include <cmath>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <limits>
#include <stdexcept>
using namespace openatc;
int checks=0;
void check(bool condition,const char* label){++checks;if(!condition)throw std::runtime_error(label);}
template<class Function> void rejects(Function action,const char* label){bool rejected=false;try{action();}catch(const std::exception&){rejected=true;}check(rejected,label);}
Request intent(const std::string& name){Request request;request.intent=name;return request;}
void acknowledge(State& state){Request request;request.intent="readback";request.altitudeFeet=state.clearance->altitudeFeet;request.waypoint=state.clearance->route;request.clearanceSequence=state.clearance->sequence;check(applyRequest(state,request).accepted,"Exact clearance readback accepted");}
void observe(State& state,const Telemetry& telemetry,int times=4){for(int index=0;index<times;++index)updateFlightPhase(state,telemetry);}
int main(){try{
    State state;state.plan.runway="09";Airport airport=demoAirport();
    check(requestAvailable(state,"clearance"),"Parked clearance offered");
    check(!requestAvailable(state,"altitude"),"Parked altitude hidden");
    check(!requestAvailable(state,"direct"),"Parked direct-to hidden");
    check(!requestAvailable(state,"descent"),"Parked descent hidden");
    check(!requestAvailable(state,"go_around"),"Parked go-around hidden");
    check(!requestAvailable(state,"gate"),"Arrival taxi hidden before landing");
    check(!requestAvailable(state,"taxi"),"Taxi hidden before clearance");
    check(!applyRequest(state,interpretText("request altitude FL320")).accepted,"Typed ground altitude rejected");
    check(!applyRequest(state,intent("taxi"),&airport).accepted,"Premature taxi rejected");
    check(applyRequest(state,intent("clearance"),&airport).accepted,"Parked clearance");
    check(!requestAvailable(state,"clearance"),"Duplicate clearance hidden");
    check(!requestAvailable(state,"pushback"),"Readback required before pushback");
    Request readback;readback.intent="readback";readback.altitudeFeet=5000;readback.waypoint="DCT";readback.clearanceSequence=999;
    check(!applyRequest(state,readback).accepted,"Stale readback rejected");readback.clearanceSequence=state.clearance->sequence;readback.altitudeFeet=6000;
    check(!applyRequest(state,readback).accepted,"Wrong altitude rejected");acknowledge(state);
    check(!applyRequest(state,readback).accepted,"No repeated acknowledgement");
    check(requestAvailable(state,"pushback"),"Pushback offered after readback");
    check(applyRequest(state,intent("pushback")).accepted,"Pushback accepted");
    check(state.phase==Phase::Pushback,"Pushback stage recorded");
    check(!requestAvailable(state,"pushback"),"Duplicate pushback hidden");
    check(applyRequest(state,intent("taxi"),&airport).accepted,"Taxi after readback");
    check(state.taxiClearance.approved && state.taxiClearance.points.size()==3,"Taxi route stored with approval");
    check(state.taxiClearance.points.back().north==200,"Route stops outside runway");
    check(state.taxiClearance.instructions.find("B, A")!=std::string::npos,"Clearance uses actual taxiway names");
    check(!requestAvailable(state,"taxi"),"Duplicate taxi hidden");
    check(applyRequest(state,intent("ready"),&airport).accepted,"Departure after taxi");
    check(!state.taxiClearance.approved,"Taxi overlay invalidated for departure");
    Telemetry airborne;airborne.onGround=false;airborne.altitudeFeet=32000;airborne.heightAglFeet=30000;airborne.groundSpeedKnots=440;airborne.positionValid=true;
    observe(state,airborne,3);check(state.phase==Phase::Departure,"Phase changes require sustained evidence");observe(state,airborne,1);check(state.phase==Phase::Cruise,"Cruise recognized");
    check(requestAvailable(state,"altitude"),"Airborne altitude offered");
    check(!requestAvailable(state,"pushback"),"Airborne pushback hidden");
    check(applyRequest(state,interpretText("request altitude FL340")).accepted,"Airborne altitude request");check(state.clearance->altitudeFeet==34000,"Flight level conversion");
    unsigned sequence=state.clearance->sequence;check(!applyRequest(state,interpretText("climb 99999")).accepted,"Invalid altitude rejected");check(state.clearance->sequence==sequence,"Rejected request preserves clearance");
    check(interpretText("unknown prose").intent=="conversation","Unknown prose cannot create clearance");check(interpretText("request direct to PELIN").waypoint=="PELIN","Direct parsing");check(interpretText("request descent FL280").intent=="descent","Descent parsing");
    acknowledge(state);state.demo=false;state.plan.fixes.push_back({"PELIN",-40,145,32000});check(!applyRequest(state,interpretText("direct to UNKNOWN")).accepted,"Direct-to unknown live fix rejected");check(applyRequest(state,interpretText("direct to PELIN")).accepted,"Known route fix accepted");
    check(!applyRequest(state,intent("cross_runway")).accepted,"No unvalidated crossing clearance");
    airborne.verticalSpeedFpm=-1500;observe(state,airborne);check(state.phase==Phase::Arrival,"Descent recognized");airborne.heightAglFeet=2500;airborne.altitudeFeet=3000;observe(state,airborne);check(state.phase==Phase::Approach,"Approach recognized");
    check(requestAvailable(state,"go_around"),"Approach go-around offered");check(applyRequest(state,intent("go_around")).accepted,"Go-around request accepted");check(state.phase==Phase::Departure,"Go-around changes stage");
    state.phase=Phase::Approach;airborne.verticalSpeedFpm=1200;observe(state,airborne);check(state.phase==Phase::Departure,"Telemetry go-around recognized");
    state.phase=Phase::Approach;Telemetry landed;landed.onGround=true;landed.groundSpeedKnots=90;observe(state,landed,1);check(state.phase==Phase::Approach,"Single bounce cannot change phase");observe(state,airborne,1);check(state.phase==Phase::Approach,"Bounce reset evidence");observe(state,landed);check(state.phase==Phase::Landed,"Landing recognized");
    check(!requestAvailable(state,"gate"),"Parking request hidden during landing roll");state.telemetry.groundSpeedKnots=12;check(requestAvailable(state,"gate"),"Parking request after landing");check(!requestAvailable(state,"altitude"),"Landing hides altitude request");
    State stationary;stationary.phase=Phase::Pushback;observe(stationary,Telemetry{},10);check(stationary.phase==Phase::Pushback,"Stationary samples preserve workflow stage");
    Telemetry paused=airborne;paused.paused=true;observe(stationary,paused,10);check(stationary.phase==Phase::Pushback,"Pause preserves stage");
    Telemetry invalid;invalid.latitude=std::numeric_limits<double>::quiet_NaN();rejects([&]{updateFlightPhase(stationary,invalid);},"NaN telemetry rejected");
    double distance=descentDistanceNm(33000,3000);check(distance>94&&distance<95,"Three-degree geometry");check(descentDistanceNm(2000,3000)==0,"No descent below target");rejects([&]{descentDistanceNm(30000,3000,0);},"Invalid descent angle");check(distanceNm(0,179,0,-179)>119&&distanceNm(0,179,0,-179)<121,"Dateline distance");
    auto weather=parseMetar("YMLT 050900Z 32012KT 9999 SCT030 14/08 Q1018");check(weather.qnh==1018&&weather.wind=="32012KT","METAR QNH and wind");check(parseMetar("KJFK 050900Z 18005KT 10SM CLR 20/10 A2992").qnh==1013,"US altimeter conversion");check(!parseMetar("INVALID").qnh.has_value(),"Unknown pressure stays unknown");
    FlightPlan plan;validateFlightPlan(plan);plan.blockFuelKg=100;plan.tripFuelKg=200;rejects([&]{validateFlightPlan(plan);},"Fuel underflow rejected");plan.tripFuelKg=0;plan.passengers=-1;rejects([&]{validateFlightPlan(plan);},"Negative passenger count rejected");plan.passengers=150;plan.departure="ymlt";rejects([&]{validateFlightPlan(plan);},"Invalid airport code rejected");plan.departure="YMLT";plan.payloadKg=std::numeric_limits<double>::infinity();rejects([&]{validateFlightPlan(plan);},"Infinite payload rejected");
    auto directory=std::filesystem::temp_directory_path()/"openatc-tests";std::filesystem::create_directories(directory);auto airportFile=directory/"apt.dat";
    {std::ofstream output(airportFile);output<<"I\n1200 test\n1 100 1 0 TEST Test Airport\n100 45 1 0 0.25 1 2 1 09 -41.0 147.0 0 0 3 0 0 0 27 -41.0 147.02 0 0 3 0 0 0\n1201 -41.001 147.002 both 1\n1201 -41.001 147.010 both 2\n1202 1 2 oneway taxiway_C Alpha\n1204 departure 09\n1300 -41.002 147.005 180 gate jets Gate 17\n1301 C airline QFA\n54 11810 Old Tower\n1054 118105 Tower\n53 12190 Ground\n1 0 0 0 NEXT Next Airport\n99\n";}
    auto loaded=loadAirport(airportFile.string(),"TEST");check(loaded.runways.size()==1,"Runway parsing");check(loaded.nodes.size()==2&&loaded.edges.size()==1,"Taxi network parsing");check(loaded.edges[0].name=="Alpha"&&loaded.edges[0].oneWay,"One-way and taxi names");check(loaded.edges[0].size=='C',"Taxi size parsed");check(!loaded.edges[0].activeRunways.empty(),"Active zone retained");check(loaded.parking.size()==1&&loaded.parking[0].name=="Gate 17","Parking names with spaces");check(loaded.parking[0].size=="C","Parking size metadata");check(loaded.frequencies.size()==1&&loaded.frequencies[0].khz==118105,"Modern frequency records supersede legacy");check(loaded.runways[0].second.east>1600,"Geographic projection");rejects([&]{loadAirport(airportFile.string(),"NONE");},"Missing airport rejected");
    {std::ofstream output(directory/"earth_nav.dat");output<<"I\n1200 Version\n3 -41.0 147.01 100 11680 130 19.0 TST ENRT YM TEST VOR\n2 -41.0 147.01 100 362 25 0 TST ENRT YM TEST NDB\n4 -41.0 147.01 100 11030 25 59220.343 ITST TEST YM 09 ILS-cat-I\n6 -41.0 147.01 100 11030 25 300180.343 ITST TEST YM 09 GS\n4 -41.0 147.01 100 11030 25 180.343 IOTH NEXT YM 09 ILS-cat-I\n3 10.0 10.0 0 11400 130 0 FAR ENRT XX FAR VOR\n99\n";}
    loadNavaids(loaded,(directory/"earth_nav.dat").string());check(loaded.navaids.size()==4,"Local navaids and airport-specific ILS filtering");check(std::abs(loaded.navaids[0].frequency-116.8)<0.001,"VOR MHz");check(loaded.navaids[1].frequency==362,"NDB kHz");check(std::abs(loaded.navaids[2].bearing-180.343)<0.001,"Encoded localizer bearing decoded");check(loaded.navaids[3].glideAngle==3,"Glideslope angle decoded");
    {std::ofstream output(directory/"TEST.dat");output<<"SID:010,5,TEST1,09,FIXA,\nSID:020,5,TEST1,09,FIXB,\nSTAR:010,4,ARR1,TRANS,FIXC,\nAPPCH:010,I,I09,FINAL,FIXD,\n";}
    loadProcedures(loaded,(directory/"TEST.dat").string());check(loaded.procedures.size()==3,"Procedure list deduplicated");check(loaded.procedures[0].transition=="09","Procedure transition retained");
    Telemetry taxi;taxi.positionValid=true;taxi.latitude=430.0/111320;taxi.longitude=0;auto route=calculateTaxiRoute(airport,taxi,"09",false);check(route.points.size()==3,"Graph route follows connected nodes");check(!route.approved,"Calculation is not clearance");
    auto disconnected=airport;disconnected.edges[4].oneWay=true;rejects([&]{calculateTaxiRoute(disconnected,taxi,"09",false);},"Reverse one-way route rejected");auto blocked=airport;blocked.edges[0].activeRunways="09";rejects([&]{calculateTaxiRoute(blocked,taxi,"09",false);},"Active runway zone blocked");auto narrow=airport;narrow.edges[0].size='B';rejects([&]{calculateTaxiRoute(narrow,taxi,"09",false,'C');},"Too-narrow edge blocked");
    State arrival;arrival.phase=Phase::TaxiIn;arrival.hasDeparted=true;arrival.taxiClearance=calculateTaxiRoute(airport,taxi,"Gate 1",true);arrival.taxiClearance.approved=true;observe(arrival,taxi);check(arrival.phase==Phase::Finished&&!arrival.taxiClearance.approved,"Arrival completes at assigned parking");
    taxi.onGround=false;rejects([&]{calculateTaxiRoute(airport,taxi,"09",false);},"Airborne taxi route rejected");taxi.onGround=true;rejects([&]{calculateTaxiRoute(airport,taxi,"Missing stand",true);},"Unknown parking rejected");
    {State voice;Airport demo=demoAirport();
    check(controllerService(voice)=="Clearance","Parked service is Clearance");
    check(controllerAirspace(voice,&demo)=="DEMO:Clearance","Demo airspace key");
    check(controllerAirspace(voice,nullptr)=="YMLT:Clearance","Plan airspace without airport");
    auto pool=parseVoicePool("af_bella, am_adam ,bf_emma,,");check(pool.size()==3&&pool[1]=="am_adam","Voice pool parsing trims and skips empties");
    check(deliveryPreset("brisk").speed>1.0f&&deliveryPreset("nope").sentencePause==0.25f,"Delivery presets with standard fallback");
    Controllers roster;std::mt19937 rng{42};
    auto first=assignController(roster,"DEMO:Tower",{"af_bella","am_adam","bf_emma"},"alloy","brisk",0.9f,1.15f,rng);
    check(roster.assignments.size()==1&&roster.recent.size()==1,"New airspace assigns and remembers");
    check(first.speed>=0.9f&&first.speed<=1.15f&&first.delivery=="brisk","Assigned speed inside range with delivery");
    auto again=assignController(roster,"DEMO:Tower",{"af_bella","am_adam","bf_emma"},"alloy","brisk",0.9f,1.15f,rng);
    check(again.voice==first.voice&&roster.assignments.size()==1,"Returning airspace keeps its controller");
    auto second=assignController(roster,"DEMO:Ground",{"af_bella","am_adam","bf_emma"},"alloy","brisk",0.9f,1.15f,rng);
    check(second.voice!=first.voice,"Neighbor airspace avoids the recent voice");
    auto tiny=assignController(roster,"DEMO:Clearance",{"af_bella"},"alloy","standard",1,1,rng);
    check(tiny.voice=="af_bella","Single-voice pool still assigns");
    auto fallback=assignController(roster,"DEMO:ATIS",{},"alloy","standard",1,1,rng);
    check(fallback.voice=="alloy","Empty pool uses the fallback voice");
    SpeechTag tag{"Approach","af_sky","brisk",1.1f,false};applyRequest(voice,intent("go_around"),&demo,tag);
    const auto& stamped=voice.transcript.back();check(stamped.speaker=="ATC"&&stamped.urgent&&stamped.position=="Approach"&&stamped.voice=="af_sky","Go-around reply stamped urgent with tag");}
    {State strict;strict.plan.callsign="N123AB";strict.telemetry.positionValid=true;strict.telemetry.latitude=1;strict.telemetry.longitude=2;strict.frequencySequence=1;strict.recommendedFrequencyKhz=118100;
    Realism tuned;strict.telemetry.com1Khz=121900;tuned.requireFrequency=true;
    check(applyRequest(strict,intent("position"),nullptr,{},{},tuned).message.find("Contact")==0,"Wrong frequency refused with contact instruction");
    strict.telemetry.com1Khz=118100;
    check(applyRequest(strict,intent("position"),nullptr,{},{},tuned).accepted,"Tuned frequency served");
    Realism named;named.requireCallsign=true;
    Request vague;vague.intent="conversation";vague.text="taxi please";
    check(applyRequest(strict,vague,nullptr,{},{},named).message.find("Say callsign")==0,"Missing callsign refused");
    vague.text="N123AB taxi please";
    check(applyRequest(strict,vague,nullptr,{},{},named).message.find("Say callsign")!=0,"Callsign present passes the gate");
    Request odd;odd.intent="weather";odd.text="request weather";
    check(applyRequest(strict,odd,nullptr,{},{},Realism()).message.find("Say again")!=std::string::npos,"Strict phraseology corrects unknown requests");
    Realism relaxed;relaxed.strictPhraseology=false;
    check(applyRequest(strict,odd,nullptr,{},{},relaxed).message.find("For example")!=std::string::npos,"Relaxed mix teaches with examples");
    Realism easy;easy.strictPhraseology=false;easy.teachingCorrections=false;
    check(applyRequest(strict,odd,nullptr,{},{},easy).message.find("Didn't catch")!=std::string::npos,"Pure relaxed guesses helpfully");
    Request crisis;crisis.intent="emergency";crisis.text="mayday mayday";
    check(!applyRequest(strict,crisis,nullptr,{},{},Realism()).accepted,"Emergency practice off refuses");
    Realism drills;drills.practiceEmergencies=true;
    check(applyRequest(strict,crisis,nullptr,{},{},drills).accepted&&strict.transcript.back().urgent,"Emergency practice on answers urgent");
    check(interpretText("pan pan, engine failure").intent=="emergency","Pan call maps to emergency");}
    {State readback;readback.plan.callsign="N123AB";readback.phase=Phase::Parked;readback.telemetry.onGround=true;
    applyRequest(readback,intent("clearance"),nullptr);
    Request partial;partial.intent="readback";partial.clearanceSequence=readback.clearance->sequence;partial.altitudeFeet=9999;partial.waypoint=readback.clearance->route;
    check(!applyRequest(readback,partial,nullptr,{},{},Realism()).accepted,"Strict readback rejects wrong altitude");
    Realism lenient;lenient.strictReadbacks=false;
    check(applyRequest(readback,partial,nullptr,{},{},lenient).accepted,"Lenient readback accepts matching sequence");}
    check(resolveUnits("imperial","YMLT")==UnitSystem::Imperial,"Imperial preference wins");
    check(resolveUnits("metric","KJFK")==UnitSystem::Metric,"Metric preference wins");
    check(resolveUnits("region","KJFK")==UnitSystem::Imperial,"US region flies imperial");
    check(resolveUnits("region","YMLT")==UnitSystem::Hybrid,"Australia region flies hybrid");
    check(resolveUnits("region","ZBAA")==UnitSystem::Metric,"China region flies metric");
    check(resolveUnits("region","")!=UnitSystem::Metric,"Empty departure stays imperial");
    check(altitudeText(32000,UnitSystem::Imperial,false)=="32000 ft","Imperial altitude display");
    check(altitudeText(32000,UnitSystem::Metric,false)=="9750 m","Metric altitude display");
    check(altitudeText(32000,UnitSystem::Metric,true)=="9750 meters","Metric altitude speech");
    check(altitudeText(32000,UnitSystem::Hybrid,false)=="32000 ft","Hybrid altitude stays feet");
    check(speedText(440,UnitSystem::Imperial,false)=="440 kt","Imperial speed display");
    check(speedText(440,UnitSystem::Metric,true)=="815 kilometers per hour","Metric speed speech");
    check(distanceText(214,UnitSystem::Imperial,false)=="214 NM","Imperial distance display");
    check(distanceText(214,UnitSystem::Metric,false)=="396 km","Metric distance display");
    check(climbRateText(500,UnitSystem::Imperial,true)=="500 feet per minute","Imperial climb speech");
    check(climbRateText(500,UnitSystem::Metric,false)=="2.5 m/s","Metric climb display");
    check(metersToFeet(3000)==9800,"Meter entry rounds to 100-foot steps");
    check(interpretText("request altitude 3000 meters").altitudeFeet==9800,"Typed meters convert to feet");
    {State metric;metric.plan.callsign="N123AB";metric.phase=Phase::Parked;metric.telemetry.onGround=true;
    check(applyRequest(metric,intent("clearance"),nullptr,{},{},{},UnitSystem::Metric).message.find("meters")!=std::string::npos,"Metric clearance speaks meters");}
    check(parseMetar("KJFK 050900Z 18005KT 10SM CLR 20/10 A2992").altimeter==2992,"US altimeter captured");
    check(pressureText(parseMetar("YMLT 050900Z 32012KT 9999 SCT030 14/08 Q1018"),builtinRegion("YMLT"),true)=="QNH 1018","QNH speech outside the US");
    check(pressureText(parseMetar("KJFK 050900Z 18005KT 10SM CLR 20/10 A2992"),builtinRegion("KJFK"),true)=="Altimeter 2992","Altimeter speech in the US");
    check(pressureText(parseMetar("KJFK 050900Z 18005KT 10SM CLR 20/10 A2992"),builtinRegion("KJFK"),false)=="29.92 inHg","Altimeter display converts");
    {std::ofstream regions(directory/"regions.toml");regions<<"[regions.us]\nprefixes = [\"K\"]\npressure = \"altimeter\"\naltitude = \"feet\"\nclearance = \"initial\"\ntransition_feet = 18000\n[regions.icao]\nprefixes = []\npressure = \"qnh\"\naltitude = \"feet\"\nclearance = \"sid\"\ntransition_feet = 10000\n";}
    auto regions=loadRegions((directory/"regions.toml").string());
    check(regionFor(regions,"KJFK").clearance=="initial","US clearance variant from file");
    check(regionFor(regions,"YMLT").clearance=="sid","ICAO clearance variant from file");
    check(regionFor(regions,"YMLT").transitionFeet==10000,"Transition altitude from file");
    rejects([&]{loadRegions((directory/"missing.toml").string());},"Missing regions file rejected");
    {State us;us.plan.callsign="N123AB";us.plan.departure="KJFK";us.plan.destination="KLAX";us.phase=Phase::Parked;us.telemetry.onGround=true;
    check(applyRequest(us,intent("clearance"),nullptr,{},{},{},UnitSystem::Imperial,regionFor(regions,"KJFK")).message.find("initial altitude")!=std::string::npos,"US clearance shape");
    State icao;icao.plan.callsign="VH-ABC";icao.plan.departure="YMLT";icao.plan.destination="YMML";icao.phase=Phase::Parked;icao.telemetry.onGround=true;
    check(applyRequest(icao,intent("clearance"),nullptr,{},{},{},UnitSystem::Imperial,regionFor(regions,"YMLT")).message.find("climb to")!=std::string::npos,"ICAO clearance shape");}
    Json calm={{"icaoId","YMLT"},{"wxString",""},{"fltCat","VFR"},{"rawOb","YMLT 050900Z 32005KT 9999 SCT030 14/08 Q1018"}};
    check(evaluateWeather(Json::array({calm}),"YMLT","YMML","",Phase::Cruise).empty(),"Calm VFR raises nothing");
    Json storm={{"icaoId","YMML"},{"wxString","TSRA"},{"fltCat","IFR"},{"rawOb","YMML 050900Z 32012G25KT 4000 TSRA SCT030 14/08 Q1018"},{"wdir",320},{"wspd",12},{"wgst",25},{"visib",4}};
    auto hazards=evaluateWeather(Json::array({storm,calm}),"YMLT","YMML","",Phase::Cruise);
    check(hazards.size()==2&&hazards[0].kind=="thunderstorm"&&hazards[1].kind=="ifr","Thunderstorm plus destination IFR flagged");
    Json offroute={{"icaoId","KJFK"},{"wxString","TSRA"},{"fltCat","LIFR"},{"rawOb","KJFK 050900Z 18005KT 1SM TSRA OVC008 20/10 A2992"}};
    check(evaluateWeather(Json::array({offroute}),"YMLT","YMML","",Phase::Cruise).empty(),"Off-route weather stays silent");
    Json shear={{"icaoId","YMML"},{"wxString","SHRA"},{"fltCat","MVFR"},{"rawOb","YMML 050900Z 32012KT 6000 SHRA WS RWY16 14/08 Q1018"}};
    check(evaluateWeather(Json::array({shear}),"YMLT","YMML","",Phase::Approach).size()==1,"Windshear flagged on approach");
    check(evaluateWeather(Json::array({shear}),"YMLT","YMML","",Phase::Cruise).empty(),"Windshear silent enroute");
    Json ice={{"icaoId","YMML"},{"wxString","FZRA"},{"fltCat","IFR"},{"rawOb","YMML 050900Z 32012KT 4000 FZRA OVC008 01/00 Q1018"}};
    check(evaluateWeather(Json::array({ice}),"YMLT","YMML","",Phase::Cruise).front().kind=="freezing_rain","Freezing rain flagged");
    {State advised;check(!advisoryKnown(advised,"YMML","thunderstorm"),"No advisory remembered initially");rememberAdvisory(advised,"YMML","thunderstorm",1);rememberAdvisory(advised,"YMML","thunderstorm",2);check(advised.weatherAdvisories.size()==1&&advisoryKnown(advised,"YMML","thunderstorm"),"Advisory remembered once");clearAdvisory(advised,"YMML","thunderstorm");check(!advisoryKnown(advised,"YMML","thunderstorm"),"Cleared hazard advises again");}
    {Json withNulls={{"intent","altitude"},{"altitudeFeet",nullptr},{"waypoint",nullptr},{"extra",Json::array({Json::object({{"a",nullptr},{"b",1}}),nullptr})}};
    Json clean=dropNulls(withNulls);
    check(!clean.contains("altitudeFeet")&&!clean.contains("waypoint"),"Null object keys dropped");
    check(clean.at("intent")=="altitude","Present keys survive");
    check(clean.at("extra").size()==2&&!clean.at("extra").at(0).contains("a"),"Nested nulls dropped, array shape kept");
    Request parsed=clean.get<Request>();check(parsed.intent=="altitude"&&parsed.altitudeFeet==0,"Nulled request parses to defaults");}
    {Json report={{"icaoId","YMML"},{"wxString","TSRA"},{"fltCat","IFR"},{"rawOb","YMML 050900Z 32012G25KT 4000 TSRA SCT030 14/08 Q1018"},{"wdir",320},{"wspd",12},{"wgst",25},{"visib",4},{"clouds",Json::array({{ {"cover","OVC"},{"base",2400} }})}};
    WeatherHazard storm{"YMML","thunderstorm","thunderstorm"}, low{"YMML","ifr","IFR conditions"}, shear{"YMML","windshear","windshear"};
    check(advisoryText("VH-BIL",report,storm,builtinRegion("YMLT"),UnitSystem::Imperial)=="VH-BIL, YMML weather: thunderstorm, wind 320 at 12 gusting 25 knots, visibility 4 miles, QNH 1018. Advise intentions.","Thunderstorm advisory sentence");
    check(advisoryText("VH-BIL",report,low,builtinRegion("YMLT"),UnitSystem::Imperial)=="VH-BIL, YMML is now IFR conditions, ceiling 2400 feet overcast, QNH 1018. Advise intentions.","IFR advisory sentence");
    check(advisoryText("VH-BIL",report,shear,builtinRegion("YMLT"),UnitSystem::Imperial)=="VH-BIL, windshear reported at YMML. Advise intentions.","Windshear advisory sentence");
    check(advisoryText("VH-BIL",report,storm,builtinRegion("YMLT"),UnitSystem::Metric)=="VH-BIL, YMML weather: thunderstorm, wind 320 at 12 gusting 25 knots, visibility 6400 meters, QNH 1018. Advise intentions.","Metric advisory sentence");}
    check(resolveCrewRole(false,false)=="atc","Quiet panel talks to ATC");
    check(resolveCrewRole(true,false)=="cabin","Attendant call routes to cabin");
    check(resolveCrewRole(false,true)=="ground","Ground call routes to ground");
    check(resolveCrewRole(true,true)=="ground","Ground call wins over attendant");
    check(canTransmit("atc",true,true),"Powered radio transmits");
    check(!canTransmit("atc",false,true),"Dead radio blocks ATC");
    check(!canTransmit("cabin",true,false),"Dead bus blocks cabin");
    check(!canTransmit("ground",true,false),"Dead bus blocks ground");
    check(canTransmit("copilot",false,false),"Copilot needs no power");
    check(canTransmit("atc",false,false)==false,"Dark cockpit blocks all but copilot");
    check(speechWorthSending(1.5,0.2,"Request taxi"),"Real utterance sends");
    check(!speechWorthSending(0.1,0.2,"Wilco"),"Bumped button drops");
    check(!speechWorthSending(1.5,0.001,"Wilco"),"Silent hold drops");
    check(!speechWorthSending(1.5,0.2,""),"Empty transcript drops");
    check(!speechWorthSending(1.5,0.2,"[BLANK_AUDIO]"),"Whisper blank drops");
    Realism relaxed{false,false,false,false,false,false},standard{true,false,false,true,true,false},real{true,true,true,true,false,true};
    check(disciplinePreset(relaxed,"off")=="relaxed","Relaxed preset matches");
    check(disciplinePreset(standard,"quiet")=="standard","Standard preset matches");
    check(disciplinePreset(real,"busy")=="real","Real preset matches");
    check(disciplinePreset(standard,"busy")=="custom","Edited toggles read custom");
    {State calm;calm.plan.callsign="N123AB";calm.phase=Phase::Parked;calm.telemetry.onGround=true;
    check(applyRequest(calm,intent("weather"),nullptr,{},{},relaxed).message.find("Didn't catch")!=std::string::npos,"Relaxed guesses helpfully");
    State strict=calm;
    check(applyRequest(strict,intent("weather"),nullptr,{},{},Realism{true,false,false,true,false,false}).message.find("Say again")==0,"Strict gives no hints");}
    {State cockpit;cockpit.plan.callsign="N123AB";cockpit.phase=Phase::Parked;cockpit.telemetry.onGround=true;
    auto accepted=applyRequest(cockpit,intent("clearance"),nullptr,{},{},{},UnitSystem::Imperial,Region{});
    check(accepted.accepted,"Clearance accepted for readback setup");
    Request readback;readback.intent="readback";readback.altitudeFeet=cockpit.clearance->altitudeFeet;readback.waypoint=cockpit.clearance->route;readback.clearanceSequence=cockpit.clearance->sequence;
    readback.text="N123AB maintaining 5000 feet";check(applyRequest(cockpit,readback,nullptr,{},{},Realism{true,true,true,true,false,true}).accepted,"Exact copilot readback passes full discipline");}
    {std::ofstream profile(directory/"toliss.toml");profile<<"[aircraft]\nname = \"ToLiss A320neo\"\nmatch_author = \"Gliding Kiwi\"\nmatch_icao = [\"A20N\", \"A21N\"]\n[aircraft.comms]\nattendant_refs = [\"AirbusFBW/purser/fwd\"]\nground_refs = [\"AirbusFBW/purser/mech\"]\nemer_action = \"ignore\"\n[aircraft.electrical]\nbat_volts_ref = \"AirbusFBW/BatVolts\"\nmin_volts = 25.5\nbattery_refs = []\ngpu_refs = []\napu_refs = []\nrmp_refs = []\navionics_refs = []\n";}
    auto aircraft=loadAircraftProfile((directory/"toliss.toml").string());
    check(aircraft.name=="ToLiss A320neo","Profile name loads");
    check(aircraftMatches(aircraft,"Gliding Kiwi","A20N"),"A20N matches ToLiss profile");
    check(!aircraftMatches(aircraft,"Laminar Research","B738"),"Other aircraft do not match");
    check(!aircraftMatches(aircraft,"Gliding Kiwi","A339"),"Unlisted variant does not match");
    check(aircraft.electrical.minVolts==25.5,"Voltage threshold loads");
    check(profileHasPowerSources(aircraft),"Profile power sources detected");
    PowerInput dead;auto noPower=evaluatePower(dead,25.5);
    check(!noPower.bus&&!noPower.radio,"Dark cockpit means no power");
    PowerInput volts;volts.batVolts=27.0;volts.hasVolts=true;
    check(evaluatePower(volts,25.5).bus,"Healthy volts power the bus");
    PowerInput radio;radio.rmp.push_back(1);
    check(evaluatePower(radio,25.5).radio&&!evaluatePower(radio,25.5).bus,"RMP alone powers radio only");
    PowerInput gpu;gpu.gpu.push_back(1);
    check(evaluatePower(gpu,25.5).bus&&!evaluatePower(gpu,25.5).radio,"GPU powers bus without radio");
    rejects([&]{loadAircraftProfile((directory/"missing.toml").string());},"Missing aircraft file rejected");
    {std::ofstream acf(directory/"a320.acf");acf<<"A\nI\n1000 Version\nP acf/_ICAO A20N\nP acf/_author Gliding Kiwi\nP acf/_descrip test\n";}
    std::string author,icao;check(readAcfIdentity((directory/"a320.acf").string(),author,icao),"ACF identity reads");
    check(author=="Gliding Kiwi"&&icao=="A20N","ACF identity has no leading space");
    check(!readAcfIdentity((directory/"missing.acf").string(),author,icao),"Missing ACF rejected");
    {std::ifstream fixtures(std::string(OPENATC_FIXTURE_DIR)+"/units.json");check(fixtures.good(),"units fixtures open");Json cases=Json::parse(fixtures);int matched=0;for(const auto& kase:cases){std::string fn=kase.at("fn");UnitSystem fixtureUnits=UnitSystem::Imperial;if(kase.contains("units"))fixtureUnits=kase.at("units")=="metric"?UnitSystem::Metric:(kase.at("units")=="hybrid"?UnitSystem::Hybrid:UnitSystem::Imperial);bool speech=kase.value("speech",false);std::string label="fixture "+fn;
    if(fn=="resolveUnits"){UnitSystem resolved=resolveUnits(kase.at("preference"),kase.at("departure"));std::string got=resolved==UnitSystem::Metric?"metric":(resolved==UnitSystem::Hybrid?"hybrid":"imperial");check(got==kase.at("expected"),label.c_str());}
    else if(fn=="altitudeText")check(altitudeText(kase.at("feet"),fixtureUnits,speech)==kase.at("expected"),label.c_str());
    else if(fn=="speedText")check(speedText(kase.at("knots"),fixtureUnits,speech)==kase.at("expected"),label.c_str());
    else if(fn=="distanceText")check(distanceText(kase.at("nm"),fixtureUnits,speech)==kase.at("expected"),label.c_str());
    else if(fn=="climbRateText")check(climbRateText(kase.at("fpm"),fixtureUnits,speech)==kase.at("expected"),label.c_str());
    else if(fn=="feetToMeters")check(std::abs(feetToMeters(kase.at("feet"))-static_cast<double>(kase.at("expected")))<1e-9,label.c_str());
    else if(fn=="metersToFeet")check(metersToFeet(kase.at("meters"))==static_cast<int>(kase.at("expected")),label.c_str());
    else throw std::runtime_error("unknown fixture "+fn);++matched;}
    check(matched==29,"all fixtures run");}
    {std::ifstream regionsFile(std::string(OPENATC_FIXTURE_DIR)+"/regions.json");check(regionsFile.good(),"regions fixtures open");Json regions=Json::parse(regionsFile);auto table=loadRegions(std::string(OPENATC_FIXTURE_DIR)+"/"+regions.at("toml").get<std::string>());int looked=0;for(const auto& kase:regions.at("lookups")){Region found=regionFor(table,kase.at("icao"));check(found.name==kase.at("name"),"fixture region name");check(found.pressure==kase.at("pressure"),"fixture region pressure");check(found.altitude==kase.at("altitude"),"fixture region altitude");check(found.clearance==kase.at("clearance"),"fixture region clearance");check(found.transitionFeet==static_cast<int>(kase.at("transition_feet")),"fixture region transition");UnitSystem effective=unitsForRegion(found);std::string units=effective==UnitSystem::Metric?"metric":(effective==UnitSystem::Hybrid?"hybrid":"imperial");check(units==kase.at("units"),"fixture region units");++looked;}
    check(looked==5,"all region lookups run");
    for(const auto& kase:regions.at("builtin")){Region found=builtinRegion(kase.at("icao"));check(found.name==kase.at("name")&&found.pressure==kase.at("pressure")&&found.altitude==kase.at("altitude")&&found.clearance==kase.at("clearance")&&found.transitionFeet==static_cast<int>(kase.at("transition_feet")),"fixture builtin region");++looked;}
    check(looked==9,"all builtin fixtures run");
    int rejected=0;for(const auto& text:regions.at("invalid")){std::string path=(std::filesystem::temp_directory_path()/"openatc_regions_bad.toml").string();{std::ofstream invalid(path);invalid<<text.get<std::string>();}bool threw=false;try{loadRegions(path);}catch(...){threw=true;}check(threw,"invalid regions file rejected");std::error_code code;std::filesystem::remove(path,code);++rejected;}
    check(rejected==5,"all invalid fixtures rejected");}
    {std::ifstream metars(std::string(OPENATC_FIXTURE_DIR)+"/metar.json");check(metars.good(),"metar fixtures open");Json fixtures=Json::parse(metars);auto optionalInt=[](const Json& value){return value.is_null()?std::optional<int>{}:std::optional<int>{static_cast<int>(value)};};int parsed=0;for(const auto& kase:fixtures.at("parses")){Weather weather=parseMetar(kase.at("raw"));check(weather.wind==kase.at("wind"),"fixture metar wind");check(weather.visibility==kase.at("visibility"),"fixture metar visibility");check(weather.clouds==kase.at("clouds"),"fixture metar clouds");check(weather.qnh==optionalInt(kase.at("qnh")),"fixture metar qnh");check(weather.altimeter==optionalInt(kase.at("altimeter")),"fixture metar altimeter");++parsed;}
    check(parsed==5,"all metar parses run");
    for(const auto& kase:fixtures.at("pressures")){Weather weather=parseMetar(kase.at("raw"));Region region=builtinRegion("");if(kase.at("region")=="us")region.pressure="altimeter";check(pressureText(weather,region,kase.at("speech"))==kase.at("expected"),"fixture pressure text");++parsed;}
    check(parsed==12,"all metar fixtures run");}
    {std::ifstream aircraftFile(std::string(OPENATC_FIXTURE_DIR)+"/aircraft.json");check(aircraftFile.good(),"aircraft fixtures open");Json aircraft=Json::parse(aircraftFile);AircraftProfile profile=loadAircraftProfile(std::string(OPENATC_FIXTURE_DIR)+"/"+aircraft.at("toml").get<std::string>());check(profile.name=="Test Bird","fixture profile name");check(profile.comms.attendantRefs.size()==1&&profile.comms.attendantRefs[0]=="test/cabin","fixture comms refs");check(profileHasPowerSources(profile),"fixture power sources present");int matched=0;for(const auto& kase:aircraft.at("match")){check(aircraftMatches(profile,kase.at("author"),kase.at("icao"))==kase.at("matches"),(std::string("fixture match ")+kase.at("icao").get<std::string>()).c_str());++matched;}check(matched==4,"all match fixtures run");for(const auto& kase:aircraft.at("power")){PowerInput input;input.batVolts=kase.at("volts");input.hasVolts=kase.at("has_volts");auto integers=[](const Json& values){std::vector<int> readings;for(const auto& value:values)readings.push_back(static_cast<int>(value));return readings;};input.battery=integers(kase.at("battery"));input.gpu=integers(kase.at("gpu"));input.apu=integers(kase.at("apu"));input.rmp=integers(kase.at("rmp"));input.avionics=integers(kase.at("avionics"));PowerState power=evaluatePower(input,profile.electrical.minVolts);check(power.radio==kase.at("radio").get<bool>()&&power.bus==kase.at("bus").get<bool>(),(std::string("fixture power ")+kase.at("name").get<std::string>()).c_str());++matched;}check(matched==9,"all power fixtures run");int rejected=0;for(const auto& text:aircraft.at("invalid")){std::string path=(std::filesystem::temp_directory_path()/"openatc_aircraft_bad.toml").string();{std::ofstream invalid(path);invalid<<text.get<std::string>();}bool threw=false;try{loadAircraftProfile(path);}catch(...){threw=true;}check(threw,"invalid aircraft file rejected");std::error_code code;std::filesystem::remove(path,code);++rejected;}    check(rejected==3,"all invalid aircraft fixtures rejected");}
    {std::ifstream intentsFile(std::string(OPENATC_FIXTURE_DIR)+"/intents.json");check(intentsFile.good(),"intents fixtures open");Json intents=Json::parse(intentsFile);int interpreted=0;for(const auto& kase:intents.at("interpret")){Request parsed=interpretText(kase.at("text"));check(parsed.intent==kase.at("intent"),(std::string("fixture intent ")+kase.at("text").get<std::string>()).c_str());check(parsed.altitudeFeet==static_cast<int>(kase.at("altitudeFeet")),(std::string("fixture altitude ")+kase.at("text").get<std::string>()).c_str());check(parsed.waypoint==kase.at("waypoint"),(std::string("fixture waypoint ")+kase.at("text").get<std::string>()).c_str());++interpreted;}
    check(interpreted==21,"all interpret fixtures run");
    for(const auto& kase:intents.at("roles")){bool attendant=kase.at("attendant"),ground=kase.at("ground");check(resolveCrewRole(attendant,ground)==kase.at("expected"),"fixture crew role");++interpreted;}
    for(const auto& kase:intents.at("gates")){bool radio=kase.at("radio"),bus=kase.at("bus");check(canTransmit(kase.at("role"),radio,bus)==static_cast<bool>(kase.at("expected")),"fixture gate");++interpreted;}
    for(const auto& kase:intents.at("presets")){Realism realism{kase.at("strictReadbacks"),kase.at("requireFrequency"),kase.at("requireCallsign"),kase.at("strictPhraseology"),kase.at("teachingCorrections"),kase.at("practiceEmergencies")};check(disciplinePreset(realism,kase.at("congestion"))==kase.at("expected"),"fixture preset");++interpreted;}
    for(const auto& kase:intents.at("worthSending")){check(speechWorthSending(kase.at("seconds"),kase.at("peak"),kase.at("text"))==static_cast<bool>(kase.at("expected")),"fixture worth sending");++interpreted;}
    check(interpreted==39,"all intents fixtures run");}
    {std::ifstream hazardsFile(std::string(OPENATC_FIXTURE_DIR)+"/hazards.json");check(hazardsFile.good(),"hazards fixtures open");Json hazards=Json::parse(hazardsFile);int evaluated=0;for(const auto& kase:hazards.at("evaluations")){auto found=evaluateWeather(kase.at("reports"),kase.at("departure"),kase.at("destination"),kase.value("alternate",std::string{}),kase.at("phase")=="Approach"?Phase::Approach:(kase.at("phase")=="Cruise"?Phase::Cruise:Phase::Parked));std::vector<std::string> kinds;for(const auto& hazard:found)kinds.push_back(hazard.kind);std::vector<std::string> expected;for(const auto& kind:kase.at("expected"))expected.push_back(kind.get<std::string>());check(kinds==expected,(std::string("fixture hazards ")+kase.at("name").get<std::string>()).c_str());++evaluated;}
    check(evaluated==8,"all hazard evaluations run");
    for(const auto& kase:hazards.at("advisories")){WeatherHazard hazard{kase.at("station"),kase.at("kind"),kase.at("summary")};Region region=builtinRegion("");if(kase.at("region")=="us")region.pressure="altimeter";UnitSystem units=kase.at("units")=="metric"?UnitSystem::Metric:UnitSystem::Imperial;check(advisoryText("VH-BIL",kase.at("report"),hazard,region,units)==kase.at("expected"),(std::string("fixture advisory ")+kase.at("name").get<std::string>()).c_str());++evaluated;}
    check(evaluated==12,"all hazard fixtures run");}
    for(int index=0;index<600;++index){addTransmission(state,"test","bounded");}
    check(state.transcript.size()==500,"Transcript retention cap");
    std::filesystem::remove_all(directory);std::cout<<checks<<" checks passed\n";return 0;
}catch(const std::exception& error){std::cerr<<"FAILED after "<<checks<<" checks: "<<error.what()<<"\n";return 1;}}
