#pragma once
#include "serialization.hpp"
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <deque>
#include <map>
#include <memory>
#include <mutex>
#include <thread>
#include <httplib.h>
#ifdef CPPHTTPLIB_OPENSSL_SUPPORT
#error UI transport must use plain HTTP. HTTPS belongs in the engine.
#endif
namespace openatc {
struct EngineReply { bool success=false;Json data;std::string error; };
class EngineClient {
    struct Command {std::string path;Json body;};
    std::mutex mutex_;std::condition_variable wake_;std::deque<Command> pending_;std::map<std::string,EngineReply> replies_;
    State snapshot_;Settings settings_;std::string status_="Connecting to engine";bool connected_=false;std::optional<Telemetry> latestTelemetry_;
    std::atomic<bool> running_{true};std::thread pollingWorker_,commandWorker_;std::string endpoint_;std::unique_ptr<httplib::Client> pollingConnection_,commandConnection_;
    void pollState() {
        while(running_) {
            std::optional<Telemetry> telemetry;{std::lock_guard<std::mutex> guard(mutex_);telemetry=latestTelemetry_;latestTelemetry_.reset();}
            if(telemetry)pollingConnection_->Post("/telemetry",Json(*telemetry).dump(),"application/json");
            auto response=pollingConnection_->Get("/state");
            {std::lock_guard<std::mutex> guard(mutex_);connected_=false;
                if(response && response->status==200){try{auto data=Json::parse(response->body);snapshot_=data.get<State>();settings_=data.value("settings",Settings{});connected_=true;status_=snapshot_.demo?"Demo controller connected":"X-Plane connected";}catch(...){status_="Incompatible engine response";}}
                else status_="Engine disconnected";
            }
            for(int interval=0;interval<8 && running_;++interval)std::this_thread::sleep_for(std::chrono::milliseconds(25));
        }
    }
    void processCommands() {
        while(running_) {
            Command command;
            {std::unique_lock<std::mutex> guard(mutex_);wake_.wait(guard,[&]{return !running_||!pending_.empty();});if(!running_)break;command=std::move(pending_.front());pending_.pop_front();}
            EngineReply reply;auto response=commandConnection_->Post(command.path,command.body.dump(),"application/json");
            if(!response)reply.error="Engine request failed; it was not replayed.";
            else {try{reply.data=Json::parse(response->body);reply.success=response->status==200;if(!reply.success)reply.error=reply.data.value("error",std::string("Engine rejected the request"));}catch(...){reply.error="Invalid engine response";}}
            std::lock_guard<std::mutex> guard(mutex_);replies_[command.path]=std::move(reply);
        }
    }
public:
    explicit EngineClient(std::string endpoint="http://127.0.0.1:8087"):endpoint_(std::move(endpoint)) {
        pollingConnection_=std::make_unique<httplib::Client>(endpoint_);pollingConnection_->set_connection_timeout(1);pollingConnection_->set_read_timeout(2);pollingConnection_->set_write_timeout(2);
        commandConnection_=std::make_unique<httplib::Client>(endpoint_);commandConnection_->set_connection_timeout(2);commandConnection_->set_read_timeout(35);commandConnection_->set_write_timeout(5);
        pollingWorker_=std::thread([this]{pollState();});commandWorker_=std::thread([this]{processCommands();});
    }
    ~EngineClient(){running_=false;wake_.notify_all();pollingConnection_->stop();commandConnection_->stop();if(pollingWorker_.joinable())pollingWorker_.join();if(commandWorker_.joinable())commandWorker_.join();}
    EngineClient(const EngineClient&)=delete;EngineClient& operator=(const EngineClient&)=delete;
    State state(){std::lock_guard<std::mutex> guard(mutex_);return snapshot_;}
    Settings settings(){std::lock_guard<std::mutex> guard(mutex_);return settings_;}
    bool connected(){std::lock_guard<std::mutex> guard(mutex_);return connected_;}
    std::string status(){std::lock_guard<std::mutex> guard(mutex_);return status_;}
    const std::string& endpoint()const{return endpoint_;}
    void post(std::string path,Json body=Json::object()){std::lock_guard<std::mutex> guard(mutex_);if(pending_.size()<32){pending_.push_back({std::move(path),std::move(body)});wake_.notify_one();}else replies_[path]={false,Json{},"Request queue full"};}
    std::optional<EngineReply> takeReply(const std::string& path){std::lock_guard<std::mutex> guard(mutex_);auto found=replies_.find(path);if(found==replies_.end())return {};auto reply=std::move(found->second);replies_.erase(found);return reply;}
    void telemetry(const Telemetry& value){std::lock_guard<std::mutex> guard(mutex_);latestTelemetry_=value;}
};
}
