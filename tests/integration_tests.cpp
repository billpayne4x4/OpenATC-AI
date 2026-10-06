#include "openatc/simbrief.hpp"
#include <iostream>
#include <stdexcept>
using namespace openatc;
int main(){try{
    Json document={{"origin",{{"icao_code","YMLT"},{"plan_rwy","32"},{"pos_lat","-41.545"},{"pos_long","147.214"}}},{"destination",{{"icao_code","YMML"},{"plan_rwy","27"},{"pos_lat","-37.673"},{"pos_long","144.843"}}},{"alternate",{{"icao_code","YMAV"}}},{"general",{{"icao_airline","QFA"},{"flight_number","123"},{"route","DCT PELIN DCT"},{"initial_altitude","32000"},{"sid_ident","TEST1"},{"star_ident","ARR1"},{"costindex","5"}}},{"aircraft",{{"icaocode","A20N"},{"reg","VH-BIL"}}},{"params",{{"units","lbs"},{"airac","2609"}}},{"weights",{{"pax_count","150"},{"payload","30000"},{"cargo","2000"},{"est_zfw","130000"},{"est_tow","145000"},{"est_ldw","135000"}}},{"fuel",{{"plan_ramp","15000"},{"enroute_burn","9000"},{"reserve","2000"},{"contingency","500"},{"alternate_burn","1500"},{"taxi","300"}}},{"times",{{"est_time_enroute","3600"}}},{"navlog",{{"fix",Json::array({{{"ident","PELIN"},{"pos_lat","-40"},{"pos_long","146"},{"altitude_feet","32000"}}})}}}};
    auto plan=parseSimBrief(document);
    if(plan.callsign!="QFA123"||plan.sid!="TEST1"||plan.star!="ARR1"||plan.passengers!=150||plan.aircraft!="A20N"||plan.alternate!="YMAV")throw std::runtime_error("Dispatch field import");
    if(std::abs(plan.blockFuelKg-6803.88555)>0.001||plan.estimatedMinutes!=60)throw std::runtime_error("Fuel/time unit normalization");
    if(plan.fixes.size()!=3||plan.fixes[1].identifier!="PELIN")throw std::runtime_error("Georeferenced route import");
    FlightPlan restored=Json(plan).get<FlightPlan>();if(restored.star!=plan.star||restored.fixes.size()!=3||restored.blockFuelKg!=plan.blockFuelKg)throw std::runtime_error("Plan JSON roundtrip");
    Settings settings;settings.copilotReplies=true;settings.inputDevice="Test microphone";Settings restoredSettings=Json(settings).get<Settings>();if(!restoredSettings.copilotReplies||restoredSettings.inputDevice!=settings.inputDevice)throw std::runtime_error("Settings roundtrip");
    State state;state.plan=plan;state.phase=Phase::Pushback;State restoredState=Json(state).get<State>();if(restoredState.phase!=Phase::Pushback||restoredState.plan.passengers!=150)throw std::runtime_error("Extended state roundtrip");
    bool rejected=false;try{parseSimBrief(Json{{"error","No flight"}});}catch(...){rejected=true;}if(!rejected)throw std::runtime_error("Error response accepted");
    document["general"]["initial_altitude"]="nan";rejected=false;try{parseSimBrief(document);}catch(...){rejected=true;}if(!rejected)throw std::runtime_error("Nonfinite SimBrief altitude accepted");
    document["general"]["initial_altitude"]=32000;document["params"]["units"]="kgs";document["navlog"]["fix"]=document["navlog"]["fix"][0];plan=parseSimBrief(document);if(plan.blockFuelKg!=15000||plan.fixes.size()!=3)throw std::runtime_error("Numeric values or single-fix object import");
    std::cout<<"9 SimBrief and serialization checks passed\n";return 0;
}catch(const std::exception& error){std::cerr<<error.what()<<'\n';return 1;}}
