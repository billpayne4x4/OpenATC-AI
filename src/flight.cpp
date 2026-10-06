#include "openatc/core.hpp"
#include <regex>
#include <stdexcept>

namespace openatc {
bool requestAvailable(const State& state,const std::string& intent) {
    bool airborne=!state.telemetry.onGround;
    bool pending=state.clearance && !state.clearance->acknowledged;
    if(intent=="radio_check" || intent=="emergency" || intent=="position")return true;
    if(intent=="repeat" || intent=="standby" || intent=="unable")return !state.transcript.empty();
    if(intent=="readback")return pending;
    if(intent=="clearance")return !airborne && state.phase==Phase::Parked && state.telemetry.groundSpeedKnots<1;
    if(intent=="pushback")return !airborne && state.phase==Phase::Clearance && state.clearance && !pending;
    if(intent=="taxi")return !airborne && (state.phase==Phase::Clearance || state.phase==Phase::Pushback) && state.clearance && !pending;
    if(intent=="ready"){
        if(airborne||state.phase!=Phase::Taxi||!state.taxiClearance.approved||state.taxiClearance.points.empty())return false;
        if(state.demo)return true;
        if(!state.telemetry.positionValid||state.telemetry.groundSpeedKnots>5)return false;
        Airport reference;reference.referenceLatitude=state.taxiClearance.referenceLatitude;reference.referenceLongitude=state.taxiClearance.referenceLongitude;
        auto position=airportPoint(reference,state.telemetry.latitude,state.telemetry.longitude);auto end=state.taxiClearance.points.back();return std::hypot(position.east-end.east,position.north-end.north)<180;
    }
    if(intent=="gate")return !airborne && state.phase==Phase::Landed && state.telemetry.groundSpeedKnots<30;
    if(intent=="progressive" || intent=="cross_runway")return !airborne && (state.phase==Phase::Taxi || state.phase==Phase::TaxiIn);
    if(intent=="altimeter" || intent=="weather" || intent=="frequency" || intent=="checkin")return state.phase!=Phase::Finished;
    if(intent=="go_around" || intent=="visual" || intent=="localizer")return airborne && state.phase==Phase::Approach;
    if(intent=="approach" || intent=="runway")return airborne && (state.phase==Phase::Arrival || state.phase==Phase::Approach);
    if(intent=="altitude" || intent=="direct" || intent=="route" || intent=="deviation" || intent=="hold" || intent=="speed" || intent=="divert" || intent=="cancel_ifr")return airborne && state.clearance.has_value();
    if(intent=="descent")return airborne && state.clearance && (state.phase==Phase::Cruise || state.phase==Phase::Arrival);
    if(intent=="flight_following" || intent=="transit" || intent=="circuits" || intent=="bearing")return airborne;
    return false;
}
void updateFlightPhase(State& state,const Telemetry& telemetry) {
    if(!std::isfinite(telemetry.latitude)||!std::isfinite(telemetry.longitude)||!std::isfinite(telemetry.altitudeFeet)||!std::isfinite(telemetry.verticalSpeedFpm)||!std::isfinite(telemetry.groundSpeedKnots)||std::abs(telemetry.latitude)>90||std::abs(telemetry.longitude)>180||telemetry.groundSpeedKnots<0)throw std::invalid_argument("Invalid telemetry");
    state.telemetry=telemetry;
    if(telemetry.paused)return;
    Phase candidate=state.phase;
    if(!telemetry.onGround) {
        state.hasDeparted=true;
        state.taxiClearance.approved=false;
        if(state.phase==Phase::Parked || state.phase==Phase::Clearance || state.phase==Phase::Pushback || state.phase==Phase::Taxi || state.phase==Phase::Landed || state.phase==Phase::TaxiIn || state.phase==Phase::Finished)candidate=Phase::Departure;
        if(state.phase==Phase::Departure && std::abs(telemetry.altitudeFeet-state.plan.cruiseFeet)<1000 && std::abs(telemetry.verticalSpeedFpm)<400)candidate=Phase::Cruise;
        if(state.phase==Phase::Cruise && telemetry.verticalSpeedFpm < -500)candidate=Phase::Arrival;
        if(state.phase==Phase::Arrival && telemetry.heightAglFeet<3000 && telemetry.verticalSpeedFpm<200)candidate=Phase::Approach;
        if(state.phase==Phase::Approach && telemetry.verticalSpeedFpm>700)candidate=Phase::Departure;
    } else if(state.hasDeparted && state.phase!=Phase::Landed && state.phase!=Phase::TaxiIn && state.phase!=Phase::Finished)candidate=Phase::Landed;
    if(telemetry.onGround && state.phase==Phase::TaxiIn && telemetry.positionValid && telemetry.groundSpeedKnots<0.5 && state.taxiClearance.toParking){
        Airport reference;reference.referenceLatitude=state.taxiClearance.referenceLatitude;reference.referenceLongitude=state.taxiClearance.referenceLongitude;
        auto position=airportPoint(reference,telemetry.latitude,telemetry.longitude);auto stand=state.taxiClearance.destinationPoint;
        if(std::hypot(position.east-stand.east,position.north-stand.north)<30)candidate=Phase::Finished;
    }
    if(candidate==state.phase){state.phaseEvidence=0;state.candidatePhase=candidate;return;}
    if(candidate!=state.candidatePhase){state.candidatePhase=candidate;state.phaseEvidence=0;}
    if(++state.phaseEvidence>=4){state.phase=candidate;state.phaseEvidence=0;if(candidate==Phase::Finished)state.taxiClearance.approved=false;}
}
void validateFlightPlan(const FlightPlan& plan) {
    static const std::regex airportCode("[A-Z0-9]{4}");
    if(!std::regex_match(plan.departure,airportCode)||!std::regex_match(plan.destination,airportCode)||(!plan.alternate.empty()&&!std::regex_match(plan.alternate,airportCode))||plan.callsign.empty()||plan.callsign.size()>30||plan.cruiseFeet<1000||plan.cruiseFeet>45000||plan.initialAltitudeFeet<1000||plan.initialAltitudeFeet>plan.cruiseFeet)throw std::invalid_argument("Check ICAO codes, callsign and initial/cruise altitudes.");
    if(plan.passengers<0 || plan.passengers>1000 || plan.costIndex<0 || plan.costIndex>999)throw std::invalid_argument("Passengers or cost index is outside the supported range.");
    for(double value:{plan.blockFuelKg,plan.tripFuelKg,plan.reserveFuelKg,plan.alternateFuelKg,plan.taxiFuelKg,plan.payloadKg,plan.cargoKg,plan.zeroFuelWeightKg,plan.takeoffWeightKg,plan.landingWeightKg,plan.estimatedMinutes})if(!std::isfinite(value)||value<0)throw std::invalid_argument("Weights, fuel and time must be finite, non-negative values.");
    if(plan.blockFuelKg>0 && plan.blockFuelKg<plan.tripFuelKg+plan.reserveFuelKg+plan.alternateFuelKg+plan.taxiFuelKg)throw std::invalid_argument("Block fuel is less than trip + reserve + alternate + taxi fuel.");
    for(const auto& fix:plan.fixes)if(!std::isfinite(fix.latitude)||!std::isfinite(fix.longitude)||!std::isfinite(fix.altitudeFeet)||std::abs(fix.latitude)>90||std::abs(fix.longitude)>180)throw std::invalid_argument("Invalid route coordinate.");
}
double distanceNm(double firstLatitude,double firstLongitude,double secondLatitude,double secondLongitude) {
    constexpr double radians=3.141592653589793/180;
    double latitudeDelta=(secondLatitude-firstLatitude)*radians,longitudeDelta=(secondLongitude-firstLongitude)*radians;
    double half=std::pow(std::sin(latitudeDelta/2),2)+std::cos(firstLatitude*radians)*std::cos(secondLatitude*radians)*std::pow(std::sin(longitudeDelta/2),2);
    return 3440.065*2*std::asin(std::sqrt(std::clamp(half,0.0,1.0)));
}
Point airportPoint(const Airport& airport,double latitude,double longitude) {
    double deltaLongitude=std::remainder(longitude-airport.referenceLongitude,360.0);
    return {deltaLongitude*111320*std::cos(airport.referenceLatitude*3.141592653589793/180),(latitude-airport.referenceLatitude)*111320,0};
}
}
