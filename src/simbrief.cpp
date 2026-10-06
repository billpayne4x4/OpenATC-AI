#include "openatc/simbrief.hpp"
#include <stdexcept>
namespace openatc {
namespace {
std::string stringValue(const Json& object,const char* key,const std::string& fallback="") {
    if(!object.is_object()||!object.contains(key)||object[key].is_null())return fallback;
    const auto& value=object[key];if(value.is_string())return value.get<std::string>();if(value.is_number())return value.dump();return fallback;
}
double numberValue(const Json& object,const char* key,double fallback=0) {
    auto value=stringValue(object,key);if(value.empty())return fallback;
    try{size_t consumed=0;double result=std::stod(value,&consumed);if(consumed!=value.size()||!std::isfinite(result))throw std::invalid_argument("Invalid SimBrief number");return result;}catch(...){throw std::invalid_argument(std::string("Invalid SimBrief field: ")+key);}
}
}
FlightPlan parseSimBrief(const Json& document) {
    if(!document.contains("origin")||!document.contains("destination")||!document.contains("general"))throw std::invalid_argument("SimBrief did not return a flight plan. Generate an OFP first.");
    FlightPlan plan;const auto& origin=document.at("origin");const auto& destination=document.at("destination");const auto& general=document.at("general");auto aircraft=document.value("aircraft",Json::object());auto weights=document.value("weights",Json::object());auto fuel=document.value("fuel",Json::object());auto params=document.value("params",Json::object());
    plan.departure=stringValue(origin,"icao_code");plan.destination=stringValue(destination,"icao_code");plan.runway=stringValue(origin,"plan_rwy");plan.arrivalRunway=stringValue(destination,"plan_rwy");plan.route=stringValue(general,"route");plan.cruiseFeet=static_cast<int>(numberValue(general,"initial_altitude",32000));
    plan.callsign=stringValue(general,"icao_airline")+stringValue(general,"flight_number");plan.registration=stringValue(aircraft,"reg");if(plan.callsign.empty())plan.callsign=plan.registration.empty()?"OPENATC":plan.registration;
    plan.aircraft=stringValue(aircraft,"icaocode","A20N");plan.sid=stringValue(general,"sid_ident");plan.star=stringValue(general,"star_ident");plan.sidTransition=stringValue(general,"sid_trans");plan.starTransition=stringValue(general,"star_trans");plan.approach=stringValue(general,"appr_ident");plan.costIndex=static_cast<int>(numberValue(general,"costindex",5));plan.airac=stringValue(params,"airac");plan.source="SimBrief";
    auto alternate=document.value("alternate",Json::object());if(alternate.is_array()&&!alternate.empty())alternate=alternate[0];plan.alternate=stringValue(alternate,"icao_code");
    auto units=stringValue(params,"units","kgs");double kilograms=(units=="lbs"||units=="LBS")?0.45359237:1.0;
    plan.blockFuelKg=numberValue(fuel,"plan_ramp")*kilograms;plan.tripFuelKg=numberValue(fuel,"enroute_burn")*kilograms;plan.reserveFuelKg=(numberValue(fuel,"reserve")+numberValue(fuel,"contingency"))*kilograms;plan.alternateFuelKg=numberValue(fuel,"alternate_burn")*kilograms;plan.taxiFuelKg=numberValue(fuel,"taxi")*kilograms;
    plan.passengers=static_cast<int>(numberValue(weights,"pax_count"));plan.payloadKg=numberValue(weights,"payload")*kilograms;plan.cargoKg=numberValue(weights,"cargo")*kilograms;plan.zeroFuelWeightKg=numberValue(weights,"est_zfw")*kilograms;plan.takeoffWeightKg=numberValue(weights,"est_tow")*kilograms;plan.landingWeightKg=numberValue(weights,"est_ldw")*kilograms;plan.estimatedMinutes=numberValue(document.value("times",Json::object()),"est_time_enroute")/60;
    auto addAirport=[&](const Json& airport){std::string latitude=stringValue(airport,"pos_lat"),longitude=stringValue(airport,"pos_long");if(!latitude.empty()&&!longitude.empty())plan.fixes.push_back({stringValue(airport,"icao_code"),numberValue(airport,"pos_lat"),numberValue(airport,"pos_long"),numberValue(airport,"elevation")});};
    addAirport(origin);auto navlog=document.value("navlog",Json::object());auto fixes=navlog.is_object()?navlog.value("fix",Json::array()):Json::array();if(fixes.is_object())fixes=Json::array({fixes});
    if(fixes.is_array())for(const auto& fix:fixes){if(stringValue(fix,"pos_lat").empty()||stringValue(fix,"pos_long").empty())continue;plan.fixes.push_back({stringValue(fix,"ident"),numberValue(fix,"pos_lat"),numberValue(fix,"pos_long"),numberValue(fix,"altitude_feet")});}
    addAirport(destination);
    validateFlightPlan(plan);return plan;
}
}
