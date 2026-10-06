#include "openatc/core.hpp"
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
    check(applyRequest(strict,odd,nullptr,{},{},relaxed).message.find("catalogue")!=std::string::npos,"Relaxed keeps the catalogue fallback");
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
    {Json report={{"icaoId","YMML"},{"wxString","TSRA"},{"fltCat","IFR"},{"rawOb","YMML 050900Z 32012G25KT 4000 TSRA SCT030 14/08 Q1018"},{"wdir",320},{"wspd",12},{"wgst",25},{"visib",4},{"clouds",Json::array({{ {"cover","OVC"},{"base",2400} }})}};
    WeatherHazard storm{"YMML","thunderstorm","thunderstorm"}, low{"YMML","ifr","IFR conditions"}, shear{"YMML","windshear","windshear"};
    check(advisoryText("VH-BIL",report,storm,builtinRegion("YMLT"),UnitSystem::Imperial)=="VH-BIL, YMML weather: thunderstorm, wind 320 at 12 gusting 25 knots, visibility 4 miles, QNH 1018. Advise intentions.","Thunderstorm advisory sentence");
    check(advisoryText("VH-BIL",report,low,builtinRegion("YMLT"),UnitSystem::Imperial)=="VH-BIL, YMML is now IFR conditions, ceiling 2400 feet overcast, QNH 1018. Advise intentions.","IFR advisory sentence");
    check(advisoryText("VH-BIL",report,shear,builtinRegion("YMLT"),UnitSystem::Imperial)=="VH-BIL, windshear reported at YMML. Advise intentions.","Windshear advisory sentence");
    check(advisoryText("VH-BIL",report,storm,builtinRegion("YMLT"),UnitSystem::Metric)=="VH-BIL, YMML weather: thunderstorm, wind 320 at 12 gusting 25 knots, visibility 6400 meters, QNH 1018. Advise intentions.","Metric advisory sentence");}
    for(int index=0;index<600;++index){addTransmission(state,"test","bounded");}
    check(state.transcript.size()==500,"Transcript retention cap");
    std::filesystem::remove_all(directory);std::cout<<checks<<" checks passed\n";return 0;
}catch(const std::exception& error){std::cerr<<"FAILED after "<<checks<<" checks: "<<error.what()<<"\n";return 1;}}
