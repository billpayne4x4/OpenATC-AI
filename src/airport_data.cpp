#include "openatc/core.hpp"
#include <filesystem>
#include <fstream>
#include <sstream>
#include <set>
#include <map>
#include <queue>
#include <limits>
#include <stdexcept>

namespace openatc {
Airport loadAirportFromSimulator(const std::string& root,const std::string& icao) {
    namespace fs=std::filesystem;
    if(root.empty())throw std::runtime_error("Set your X-Plane folder in Settings.");
    std::vector<fs::path> paths;
    std::ifstream scenery(fs::path(root)/"Custom Scenery/scenery_packs.ini");
    std::string line;
    while(std::getline(scenery,line))if(line.rfind("SCENERY_PACK ",0)==0){fs::path path=line.substr(13);if(path.is_relative())path=fs::path(root)/path;paths.push_back(path/"Earth nav data/apt.dat");}
    paths.push_back(fs::path(root)/"Global Scenery/Global Airports/Earth nav data/apt.dat");
    paths.push_back(fs::path(root)/"Resources/default scenery/default apt dat/Earth nav data/apt.dat");
    std::optional<Airport> airport;
    for(const auto& path:paths)if(fs::exists(path)){try{airport=loadAirport(path.string(),icao);break;}catch(const std::exception& error){if(std::string(error.what()).find("identifier not found")==std::string::npos)throw;}}
    if(!airport)throw std::runtime_error("Airport not found in enabled scenery or Global Airports.");
    auto data=fs::path(root)/"Custom Data";
    if(!fs::exists(data/"earth_nav.dat"))data=fs::path(root)/"Resources/default data";
    if(fs::exists(data/"earth_nav.dat"))loadNavaids(*airport,(data/"earth_nav.dat").string());
    auto procedures=fs::path(root)/"Custom Data/CIFP"/(icao+".dat");
    if(!fs::exists(procedures))procedures=fs::path(root)/"Resources/default data/CIFP"/(icao+".dat");
    if(fs::exists(procedures))loadProcedures(*airport,procedures.string());
    return *airport;
}
void loadNavaids(Airport& airport,const std::string& path) {
    std::ifstream input(path);if(!input)throw std::runtime_error("Cannot open earth_nav.dat");
    std::string line;
    while(std::getline(input,line)) {
        std::istringstream row(line);int kind=0;double latitude,longitude,elevation,frequency,range,bearing;std::string identifier,airportCode,region,name;
        if(!(row>>kind>>latitude>>longitude>>elevation>>frequency>>range>>bearing>>identifier>>airportCode>>region))continue;
        if(kind!=2 && kind!=3 && kind!=4 && kind!=5 && kind!=6 && kind!=7 && kind!=8 && kind!=9 && kind!=12 && kind!=13)continue;
        bool airportSpecific=kind>=4&&kind<=9;
        if(airportSpecific && airportCode!=airport.icao)continue;
        if(!airportSpecific && distanceNm(airport.referenceLatitude,airport.referenceLongitude,latitude,longitude)>80)continue;
        std::string runway;
        if(airportSpecific)row>>runway;
        std::getline(row,name);name.erase(0,name.find_first_not_of(" \t"));
        std::string type=kind==2?"NDB":kind==3?"VOR":kind==4?"ILS LOC":kind==5?"LOC":kind==6?"GS":kind==7?"OM":kind==8?"MM":kind==9?"IM":"DME";
        double glideAngle=kind==6?std::floor(bearing/1000)/100.0:0;double decodedBearing=(kind==4||kind==5)?std::fmod(bearing,360.0):kind==6?std::fmod(bearing,1000.0):bearing;
        airport.navaids.push_back({type,identifier,name,airportCode,runway,latitude,longitude,kind==2?frequency:frequency/100.0,decodedBearing,elevation,range,glideAngle});
    }
}
void loadProcedures(Airport& airport,const std::string& path) {
    std::ifstream input(path);if(!input)throw std::runtime_error("Cannot open CIFP data");
    std::set<std::string> seen;std::string line;
    while(std::getline(input,line)) {
        auto separator=line.find(':');if(separator==std::string::npos)continue;
        std::string type=line.substr(0,separator);if(type!="SID" && type!="STAR" && type!="APPCH")continue;
        std::istringstream fields(line.substr(separator+1));std::string sequence,routeType,name,transition;
        if(!std::getline(fields,sequence,',')||!std::getline(fields,routeType,',')||!std::getline(fields,name,',')||!std::getline(fields,transition,','))continue;
        auto trim=[](std::string& value){auto start=value.find_first_not_of(" \t");if(start==std::string::npos){value.clear();return;}value=value.substr(start,value.find_last_not_of(" \t")-start+1);};trim(name);trim(transition);
        if(seen.insert(type+"/"+name+"/"+transition).second)airport.procedures.push_back({type,name,transition});
    }
}
TaxiClearance calculateTaxiRoute(const Airport& airport,const Telemetry& telemetry,const std::string& destination,bool toParking,char aircraftSize) {
    if(!telemetry.onGround || !telemetry.positionValid)throw std::runtime_error("Taxi routing needs a valid ground position.");
    if(airport.nodes.empty())throw std::runtime_error("No taxi network is available for this airport.");
    Point startPoint=airportPoint(airport,telemetry.latitude,telemetry.longitude),targetPoint;
    if(toParking){auto parking=std::find_if(airport.parking.begin(),airport.parking.end(),[&](const Parking& value){return value.name==destination;});if(parking==airport.parking.end())throw std::runtime_error("Select an arrival parking location in Flight Plan or Taxi.");targetPoint=parking->point;}
    else {bool found=false;for(const auto& runway:airport.runways){if(runway.firstName==destination){targetPoint=runway.first;found=true;}if(runway.secondName==destination){targetPoint=runway.second;found=true;}}if(!found)throw std::runtime_error("Departure runway not found in the loaded airport.");}
    auto distance=[](const Point& first,const Point& second){return std::hypot(first.east-second.east,first.north-second.north);};
    auto withinRunway=[&](const Point& point){for(const auto& runway:airport.runways){double east=runway.second.east-runway.first.east,north=runway.second.north-runway.first.north,length=std::hypot(east,north);if(length<1)continue;double along=((point.east-runway.first.east)*east+(point.north-runway.first.north)*north)/length;double across=std::abs((point.east-runway.first.east)*north-(point.north-runway.first.north)*east)/length;if(along>=-40 && along<=length+40 && across<runway.width/2+35)return true;}return false;};
    std::map<long,Point> nodes;for(const auto& node:airport.nodes)nodes[node.id]=node.point;
    struct Connection {long target;double distance;std::string name;};std::map<long,std::vector<Connection>> graph;
    for(const auto& edge:airport.edges) {
        if(edge.runway || !edge.activeRunways.empty() || edge.size<aircraftSize || !nodes.count(edge.first) || !nodes.count(edge.second))continue;
        auto first=nodes.at(edge.first),second=nodes.at(edge.second);bool conflict=false;
        int steps=std::max(1,static_cast<int>(distance(first,second)/15));
        for(int step=0;step<=steps;++step){double fraction=static_cast<double>(step)/steps;if(withinRunway({first.east+(second.east-first.east)*fraction,first.north+(second.north-first.north)*fraction})){conflict=true;break;}}
        if(conflict)continue;
        graph[edge.first].push_back({edge.second,distance(first,second),edge.name});if(!edge.oneWay)graph[edge.second].push_back({edge.first,distance(first,second),edge.name});
    }
    if(withinRunway(startPoint))throw std::runtime_error("Vacate the runway before requesting a ground taxi route.");
    long start=-1;double nearest=1e100;for(const auto& pair:nodes)if(!withinRunway(pair.second)&&distance(startPoint,pair.second)<nearest){nearest=distance(startPoint,pair.second);start=pair.first;}
    if(start<0 || nearest>350)throw std::runtime_error("No safe taxi-network start within 350 m. Move onto the taxi network or check airport data.");
    using QueueItem=std::pair<double,long>;std::priority_queue<QueueItem,std::vector<QueueItem>,std::greater<QueueItem>> pending;
    std::map<long,double> costs;std::map<long,std::pair<long,std::string>> previous;costs[start]=0;pending.push({0,start});
    while(!pending.empty()){auto [cost,node]=pending.top();pending.pop();if(cost!=costs[node])continue;for(const auto& edge:graph[node]){double next=cost+edge.distance;if(!costs.count(edge.target)||next<costs[edge.target]){costs[edge.target]=next;previous[edge.target]={node,edge.name};pending.push({next,edge.target});}}}
    long target=-1;double targetDistance=1e100;for(const auto& pair:costs){double separation=distance(nodes.at(pair.first),targetPoint);if(separation<targetDistance){target=pair.first;targetDistance=separation;}}
    if(target<0 || targetDistance>(toParking?350:500))throw std::runtime_error("No connected route to the destination without a runway/active-zone crossing. Hold position.");
    TaxiClearance clearance;clearance.airport=airport.icao;clearance.destination=destination;clearance.referenceLatitude=airport.referenceLatitude;clearance.referenceLongitude=airport.referenceLongitude;clearance.destinationPoint=targetPoint;clearance.toParking=toParking;
    std::vector<std::string> names;
    for(long node=target;;){clearance.points.push_back(nodes.at(node));if(node==start)break;auto entry=previous.at(node);names.push_back(entry.second);node=entry.first;}
    std::reverse(clearance.points.begin(),clearance.points.end());std::reverse(names.begin(),names.end());
    std::string via,last;for(const auto& name:names)if(!name.empty() && name!=last){if(!via.empty())via+=", ";via+=name;last=name;}
    if(via.empty())via="the marked network";
    clearance.instructions="Taxi via "+via+(toParking?", toward "+destination+". Stop at the end of the marked network; ramp guidance unavailable.":", hold before runway "+destination+" at the end of the marked route.");
    return clearance;
}
}
