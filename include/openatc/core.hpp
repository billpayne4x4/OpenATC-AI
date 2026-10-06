#pragma once
#include <algorithm>
#include <cmath>
#include <map>
#include <optional>
#include <random>
#include <string>
#include <vector>
#include <nlohmann/json.hpp>

namespace openatc {
using Json=nlohmann::json;
enum class Phase { Parked, Clearance, Taxi, Departure, Cruise, Arrival, Approach, Landed, Pushback, TaxiIn, Finished };
struct Telemetry {
    double latitude=0, longitude=0, altitudeFeet=0, groundSpeedKnots=0, headingDegrees=0;
    bool onGround=true, paused=false;
    double verticalSpeedFpm=0, heightAglFeet=0;
    int com1Khz=0;
    bool positionValid=false;
};
struct RouteFix { std::string identifier; double latitude=0, longitude=0, altitudeFeet=0; };
struct FlightPlan {
    std::string callsign="VH-BIL", departure="YMLT", destination="YMML", route="DCT", runway="32", arrivalRunway="27";
    int cruiseFeet=32000;
    std::string alternate, aircraft="A20N", registration="VH-BIL", sid, sidTransition, star, starTransition, approach, departureStand, arrivalStand, airac, source="Manual";
    int initialAltitudeFeet=5000, passengers=0, costIndex=5;
    double blockFuelKg=0, tripFuelKg=0, reserveFuelKg=0, alternateFuelKg=0, taxiFuelKg=0, payloadKg=0, cargoKg=0, zeroFuelWeightKg=0, takeoffWeightKg=0, landingWeightKg=0, estimatedMinutes=0;
    std::vector<RouteFix> fixes;
};
struct Clearance { int altitudeFeet=0; std::string route, runway, squawk="2000"; bool acknowledged=false; unsigned sequence=0; int frequencyKhz=0; };
struct Transmission { std::string speaker, text; unsigned sequence=0; std::string position, voice, delivery="standard"; float speed=1; bool urgent=false; };
struct Request { std::string intent, text; int altitudeFeet=0; std::string waypoint; unsigned clearanceSequence=0; std::string role="atc"; };
// Discipline rules for a request. Defaults reproduce the classic lenient controller.
struct Realism { bool strictReadbacks=true, requireFrequency=false, requireCallsign=false, strictPhraseology=true, teachingCorrections=true, practiceEmergencies=false; };
// Effective display/speech units. Hybrid flies feet aloft; pressure and visibility stay regional.
enum class UnitSystem { Imperial, Metric, Hybrid };
UnitSystem resolveUnits(const std::string& preference,const std::string& departureIcao);
// Local procedure for one departure region. pressure is "qnh" or "altimeter",
// altitude "feet" or "meters", clearance "initial" (US) or "sid" (ICAO).
struct Region { std::string name="icao", pressure="qnh", altitude="feet", clearance="sid"; int transitionFeet=10000; };
UnitSystem unitsForRegion(const Region& region);
double feetToMeters(double feet);
int metersToFeet(int meters);
std::string altitudeText(double feet,UnitSystem units,bool speech);
std::string speedText(double knots,UnitSystem units,bool speech);
std::string distanceText(double nm,UnitSystem units,bool speech);
std::string climbRateText(double fpm,UnitSystem units,bool speech);
// Voice/appearance tag stamped onto transmissions at creation time.
struct SpeechTag { std::string position, voice, delivery="standard"; float speed=1; bool urgent=false; };
// Remembered controller for one airspace ("ICAO:Service").
struct Controller { std::string voice, delivery="standard"; float speed=1; };
struct Controllers { std::map<std::string,Controller> assignments; std::vector<std::string> recent; };
struct DeliveryPreset { float speed=1, sentencePause=0.25f, clausePause=0.1f; };
struct Point { double east=0, north=0, height=0; };
struct Runway { std::string firstName, secondName; Point first, second; double width=45; };
struct TaxiNode { long id=0; Point point; };
struct TaxiEdge { long first=0, second=0; std::string name; bool runway=false, oneWay=false; std::string activeRunways; char size='F'; };
struct Parking { std::string name, type, equipment, size, operations; Point point; double heading=0; };
struct Frequency { std::string service, name; int khz=0; };
struct Navaid { std::string type, identifier, name, airport, runway; double latitude=0, longitude=0, frequency=0, bearing=0, elevationFeet=0, rangeNm=0, glideAngle=0; };
struct Procedure { std::string type, name, transition; };
struct Airport {
    std::string icao, name, source;
    double referenceLatitude=0, referenceLongitude=0, elevationFeet=0;
    std::vector<Runway> runways;
    std::vector<TaxiNode> nodes;
    std::vector<TaxiEdge> edges;
    std::vector<Parking> parking;
    std::vector<Frequency> frequencies;
    std::vector<Navaid> navaids;
    std::vector<Procedure> procedures;
};
struct TaxiClearance { std::string airport, destination, instructions; std::vector<Point> points; bool approved=false; unsigned sequence=0; double referenceLatitude=0, referenceLongitude=0; Point destinationPoint; bool toParking=false; };
struct WeatherAdvisory { std::string station, hazard; double observed=0; };
struct State {
    FlightPlan plan; Telemetry telemetry; Phase phase=Phase::Parked;
    std::optional<Clearance> clearance; std::vector<Transmission> transcript;
    bool demo=true, ifr=true; unsigned nextSequence=1;
    bool hasDeparted=false; int phaseEvidence=0; Phase candidatePhase=Phase::Parked;
    TaxiClearance taxiClearance;
    int recommendedFrequencyKhz=0; unsigned frequencySequence=0;
    std::vector<WeatherAdvisory> weatherAdvisories;
};
struct Result { bool accepted; std::string message; };
struct RequestDefinition { const char* intent; const char* title; const char* category; bool parameter=false; };
const std::vector<RequestDefinition>& requestDefinitions();
std::string phaseName(Phase phase);
bool requestAvailable(const State& state,const std::string& intent);
void updateFlightPhase(State& state,const Telemetry& telemetry);
Request interpretText(const std::string& text);
Result applyRequest(State& state,const Request& request,const Airport* airport=nullptr,const SpeechTag& atcTag={},const SpeechTag& pilotTag={},const Realism& realism={},UnitSystem units=UnitSystem::Imperial,const Region& region=Region{});
void addTransmission(State& state,const std::string& speaker,const std::string& text,const SpeechTag& tag={});
std::string controllerService(const State& state);
std::string controllerAirspace(const State& state,const Airport* airport=nullptr);
std::vector<std::string> parseVoicePool(const std::string& pool);
DeliveryPreset deliveryPreset(const std::string& name);
Controller assignController(Controllers& roster,const std::string& key,const std::vector<std::string>& pool,const std::string& fallbackVoice,const std::string& delivery,float speedMin,float speedMax,std::mt19937& rng);
void validateFlightPlan(const FlightPlan& plan);
double descentDistanceNm(double altitudeFeet,double targetFeet,double angleDegrees=3.0);
double distanceNm(double firstLatitude,double firstLongitude,double secondLatitude,double secondLongitude);
Point airportPoint(const Airport& airport,double latitude,double longitude);
Airport demoAirport();
Airport loadAirport(const std::string& path,const std::string& icao);
Airport loadAirportFromSimulator(const std::string& root,const std::string& icao);
void loadNavaids(Airport& airport,const std::string& path);
void loadProcedures(Airport& airport,const std::string& path);
TaxiClearance calculateTaxiRoute(const Airport& airport,const Telemetry& telemetry,const std::string& destination,bool toParking,char aircraftSize='C');
struct Weather { std::string raw, source="Manually entered METAR", wind="Unavailable", visibility="Unavailable", clouds="Unavailable"; std::optional<int> qnh, altimeter; };
Weather parseMetar(const std::string& raw);
std::map<std::string,Region> loadRegions(const std::string& path);
Region regionFor(const std::map<std::string,Region>& table,const std::string& icao);
Region builtinRegion(const std::string& icao);
std::string pressureText(const Weather& weather,const Region& region,bool speech);
std::string regionNotes(const Region& region,UnitSystem units);
struct WeatherHazard { std::string station, kind, summary; };
std::vector<WeatherHazard> evaluateWeather(const Json& reports,const std::string& departure,const std::string& destination,const std::string& alternate,Phase phase);
bool advisoryKnown(const State& state,const std::string& station,const std::string& hazard);
void rememberAdvisory(State& state,const std::string& station,const std::string& hazard,double observed);
void clearAdvisory(State& state,const std::string& station,const std::string& hazard);
std::string reportWord(const Json& report,const char* key);
double reportNumber(const Json& report,const char* key);
std::string advisoryText(const std::string& callsign,const Json& report,const WeatherHazard& hazard,const Region& region,UnitSystem units);
}
