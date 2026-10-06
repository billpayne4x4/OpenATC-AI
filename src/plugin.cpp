#include "openatc/branding.hpp"
#include "openatc/ui.hpp"
#include <XPLMPlugin.h>
#include <XPLMDisplay.h>
#include <XPLMGraphics.h>
#include <XPLMProcessing.h>
#include <XPLMDataAccess.h>
#include <XPLMUtilities.h>
#include <XPLMMenus.h>
#if LIN
#include <glad/gl.h>
#include <dlfcn.h>
#elif APL
#include <OpenGL/gl.h>
#else
#if IBM
#include <windows.h>
#endif
#include <GL/gl.h>
#endif
#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <memory>
#include <cmath>
#include <string>
#if LIN || APL
#include <cerrno>
#include <fcntl.h>
#include <unistd.h>
#include <sys/wait.h>
#endif
#ifdef CPPHTTPLIB_OPENSSL_SUPPORT
#error The X-Plane plugin must not link OpenSSL. Route HTTPS through the engine.
#endif

namespace {
XPLMWindowID windowId=nullptr;XPLMMenuID menuId=nullptr;int menuIndex=-1;XPLMCommandRef toggleCommand=nullptr;
ImGuiContext* context=nullptr;std::unique_ptr<openatc::EngineClient> engine;std::unique_ptr<openatc::Interface> interface;GLuint fontTexture=0;
XPLMDataRef latitudeRef,longitudeRef,altitudeRef,speedRef,headingRef,groundRef,pauseRef,verticalSpeedRef,aglRef,com1Ref;
unsigned lastFrequencySequence=0,lastStateSequence=0;
#if LIN
void* graphicsLibrary=nullptr;
bool graphicsReady=false;
GLADapiproc resolveGraphicsFunction(const char* name){return reinterpret_cast<GLADapiproc>(dlsym(graphicsLibrary,name));}
#endif
bool enabled=false;auto previousFrame=std::chrono::steady_clock::now();
std::string engineBinaryPath;bool disconnectedTiming=false;std::chrono::steady_clock::time_point disconnectedSince{},lastSpawnAttempt{};int spawnFailures=0;bool binaryMissingLogged=false;
std::string findEngineBinary(){
    if(!engineBinaryPath.empty())return engineBinaryPath;
    char pluginPath[4096]{};XPLMGetPluginInfo(XPLMGetMyID(),nullptr,pluginPath,nullptr,nullptr);
    std::filesystem::path folder=std::filesystem::path(pluginPath).parent_path().parent_path();
#if IBM
    folder/="bin";folder/="open-atc-engine.exe";
#else
    folder/="bin";folder/="open-atc-engine";
#endif
    engineBinaryPath=folder.string();return engineBinaryPath;
}
std::string engineLogPath(){
#if IBM
    if(const char* appdata=std::getenv("APPDATA"))return (std::filesystem::path(appdata)/"openatc"/"engine.log").string();
#else
    if(const char* home=std::getenv("HOME"))return (std::filesystem::path(home)/".config"/"openatc"/"engine.log").string();
#endif
    return {};
}
bool spawnEngine(const char* binary,const char* log){
#if LIN || APL
    pid_t first=fork();if(first<0)return false;
    if(first==0){
        if(fork()!=0)_exit(0);
        setsid();
        if(log&&log[0]){int logFile=open(log,O_WRONLY|O_CREAT|O_APPEND,0644);if(logFile>=0){dup2(logFile,STDOUT_FILENO);dup2(logFile,STDERR_FILENO);if(logFile>STDERR_FILENO)close(logFile);}}
#if defined(__linux__)
        close_range(3,~0U,0);
#elif defined(__APPLE__)
        closefrom(3);
#endif
        execl(binary,binary,(char*)nullptr);
        _exit(127);
    }
    int status=0;while(waitpid(first,&status,0)<0&&errno==EINTR){}
    return WIFEXITED(status)&&WEXITSTATUS(status)==0;
#elif IBM
    std::string log=engineLogPath();HANDLE logHandle=INVALID_HANDLE_VALUE;
    if(!log.empty()){std::filesystem::create_directories(std::filesystem::path(log).parent_path());logHandle=CreateFileA(log.c_str(),GENERIC_WRITE,FILE_SHARE_READ,nullptr,OPEN_ALWAYS,FILE_ATTRIBUTE_NORMAL,nullptr);if(logHandle!=INVALID_HANDLE_VALUE)SetFilePointer(logHandle,0,nullptr,FILE_END);}
    STARTUPINFOA startup{};startup.cb=sizeof(startup);
    if(logHandle!=INVALID_HANDLE_VALUE){startup.dwFlags=STARTF_USESTDHANDLES;startup.hStdOutput=logHandle;startup.hStdError=logHandle;}
    std::string command="\""+binary+"\"";PROCESS_INFORMATION info{};
    bool launched=CreateProcessA(nullptr,command.data(),nullptr,nullptr,FALSE,CREATE_NO_WINDOW|DETACHED_PROCESS,nullptr,nullptr,&startup,&info)!=0;
    if(launched){CloseHandle(info.hProcess);CloseHandle(info.hThread);}
    if(logHandle!=INVALID_HANDLE_VALUE)CloseHandle(logHandle);
    return launched;
#else
    return false;
#endif
}
void ensureEngineRunning(){
    if(!engine)return;
    if(engine->connected()){disconnectedTiming=false;spawnFailures=0;return;}
    auto now=std::chrono::steady_clock::now();
    if(!disconnectedTiming){disconnectedTiming=true;disconnectedSince=now;return;}
    if(now-disconnectedSince<std::chrono::seconds(5))return;
    double backoff=std::min(60.0,5.0*(1<<std::min(spawnFailures,3)));
    if(now-lastSpawnAttempt<std::chrono::seconds(static_cast<int>(backoff)))return;
    lastSpawnAttempt=now;
    std::string binary=findEngineBinary();std::error_code code;
    if(!std::filesystem::is_regular_file(binary,code)){if(!binaryMissingLogged){XPLMDebugString("OpenATC AI: engine binary missing, start open-atc-engine manually\n");binaryMissingLogged=true;}return;}
    binaryMissingLogged=false;
    std::string log=engineLogPath();
    if(!log.empty()){std::error_code directories;std::filesystem::create_directories(std::filesystem::path(log).parent_path(),directories);}
    if(spawnEngine(binary.c_str(),log.c_str()))spawnFailures=0;
    else{spawnFailures++;XPLMDebugString("OpenATC AI: engine launch failed\n");}
}
#if LIN
void setClipboardText(void*,const char* text){FILE* pipe=popen("wl-copy 2>/dev/null","w");if(!pipe)return;fwrite(text,1,std::strlen(text),pipe);pclose(pipe);}
const char* getClipboardText(void*){static std::string cached;cached.clear();FILE* pipe=popen("wl-paste 2>/dev/null","r");if(pipe){char chunk[4096];size_t count;while((count=fread(chunk,1,sizeof(chunk),pipe))>0)cached.append(chunk,count);pclose(pipe);if(!cached.empty()&&cached.back()=='\n')cached.pop_back();}return cached.c_str();}
#endif
struct ContextScope { ImGuiContext* previous=ImGui::GetCurrentContext(); ContextScope(){ImGui::SetCurrentContext(context);} ~ContextScope(){ImGui::SetCurrentContext(previous);} };
void mousePosition(int horizontal,int vertical){ContextScope contextScope;int left,top,right,bottom;XPLMGetWindowGeometry(windowId,&left,&top,&right,&bottom);ImGui::GetIO().AddMousePosEvent(static_cast<float>(horizontal-left),static_cast<float>(top-vertical));}
ImVec2 screenPoint(float horizontal,float vertical,const GLfloat* model,const GLfloat* projection,const GLint* viewport) {
    float input[]={horizontal,vertical,0,1},eye[4]{},clip[4]{};
    for(int row=0;row<4;++row)for(int column=0;column<4;++column)eye[row]+=model[column*4+row]*input[column];
    for(int row=0;row<4;++row)for(int column=0;column<4;++column)clip[row]+=projection[column*4+row]*eye[column];
    return {viewport[0]+(clip[0]/clip[3]+1)*viewport[2]/2,viewport[1]+(clip[1]/clip[3]+1)*viewport[3]/2};
}
void drawWindow(XPLMWindowID,void*) {
    if(!enabled || !interface)return;
#if LIN
    if(!graphicsReady){
        if(!graphicsLibrary)graphicsLibrary=dlopen("libOpenGL.so.0",RTLD_NOW|RTLD_LOCAL);
        if(!graphicsLibrary || !gladLoadGL(resolveGraphicsFunction)){XPLMDebugString("OpenATC AI: libOpenGL.so.0 or current compatibility context unavailable\n");XPLMSetWindowIsVisible(windowId,0);return;}
        graphicsReady=true;
    }
#endif
    ContextScope contextScope;int left,top,right,bottom;XPLMGetWindowGeometry(windowId,&left,&top,&right,&bottom);
    auto& input=ImGui::GetIO();input.DisplaySize={static_cast<float>(right-left),static_cast<float>(top-bottom)};auto now=std::chrono::steady_clock::now();input.DeltaTime=std::clamp(std::chrono::duration<float>(now-previousFrame).count(),0.001f,0.1f);previousFrame=now;
    int mouseHorizontal,mouseVertical;XPLMGetMouseLocationGlobal(&mouseHorizontal,&mouseVertical);mousePosition(mouseHorizontal,mouseVertical);
    glPushAttrib(GL_ALL_ATTRIB_BITS);
    if(!fontTexture){unsigned char* pixels;int width,height;input.Fonts->GetTexDataAsRGBA32(&pixels,&width,&height);glGenTextures(1,&fontTexture);glBindTexture(GL_TEXTURE_2D,fontTexture);glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MIN_FILTER,GL_LINEAR);glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MAG_FILTER,GL_LINEAR);GLint unpackRowLength;glGetIntegerv(GL_UNPACK_ROW_LENGTH,&unpackRowLength);glPixelStorei(GL_UNPACK_ROW_LENGTH,0);glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA,width,height,0,GL_RGBA,GL_UNSIGNED_BYTE,pixels);glPixelStorei(GL_UNPACK_ROW_LENGTH,unpackRowLength);input.Fonts->SetTexID(static_cast<ImTextureID>(fontTexture));}
    ImGui::NewFrame();interface->draw({0,0},input.DisplaySize);ImGui::Render();
    XPLMSetGraphicsState(0,1,0,0,1,0,0);glEnable(GL_BLEND);glBlendFunc(GL_SRC_ALPHA,GL_ONE_MINUS_SRC_ALPHA);glDisable(GL_CULL_FACE);glEnable(GL_SCISSOR_TEST);glTexEnvi(GL_TEXTURE_ENV,GL_TEXTURE_ENV_MODE,GL_MODULATE);
    GLfloat model[16],projection[16];GLint viewport[4];glGetFloatv(GL_MODELVIEW_MATRIX,model);glGetFloatv(GL_PROJECTION_MATRIX,projection);glGetIntegerv(GL_VIEWPORT,viewport);
    auto* data=ImGui::GetDrawData();for(int listIndex=0;listIndex<data->CmdListsCount;++listIndex){auto* list=data->CmdLists[listIndex];for(auto& command:list->CmdBuffer){if(command.UserCallback){if(command.UserCallback!=ImDrawCallback_ResetRenderState)command.UserCallback(list,&command);continue;}
        auto lower=screenPoint(left+command.ClipRect.x,top-command.ClipRect.w,model,projection,viewport);auto upper=screenPoint(left+command.ClipRect.z,top-command.ClipRect.y,model,projection,viewport);glScissor(static_cast<int>(lower.x),static_cast<int>(lower.y),std::max(0,static_cast<int>(upper.x-lower.x)),std::max(0,static_cast<int>(upper.y-lower.y)));glBindTexture(GL_TEXTURE_2D,static_cast<GLuint>(command.GetTexID()));
        glBegin(GL_TRIANGLES);for(unsigned index=0;index<command.ElemCount;++index){auto& vertex=list->VtxBuffer[list->IdxBuffer[command.IdxOffset+index]+command.VtxOffset];auto color=vertex.col;glColor4ub(color&255,(color>>8)&255,(color>>16)&255,(color>>24)&255);glTexCoord2f(vertex.uv.x,vertex.uv.y);glVertex2f(left+vertex.pos.x,top-vertex.pos.y);}glEnd();}}
    glPopAttrib();
}
int mouseClick(XPLMWindowID,int horizontal,int vertical,XPLMMouseStatus status,void*){ContextScope contextScope;mousePosition(horizontal,vertical);ImGui::GetIO().AddMouseButtonEvent(0,status!=xplm_MouseUp);if(status==xplm_MouseDown)XPLMTakeKeyboardFocus(windowId);return 1;}
int rightClick(XPLMWindowID,int horizontal,int vertical,XPLMMouseStatus status,void*){ContextScope contextScope;mousePosition(horizontal,vertical);ImGui::GetIO().AddMouseButtonEvent(1,status!=xplm_MouseUp);return 1;}
int mouseWheel(XPLMWindowID,int horizontal,int vertical,int wheel,int clicks,void*){ContextScope contextScope;mousePosition(horizontal,vertical);ImGui::GetIO().AddMouseWheelEvent(wheel?static_cast<float>(clicks):0,wheel?0:static_cast<float>(clicks));return 1;}
XPLMCursorStatus cursor(XPLMWindowID,int horizontal,int vertical,void*){mousePosition(horizontal,vertical);return xplm_CursorDefault;}
void keyboard(XPLMWindowID,char character,XPLMKeyFlags flags,char virtualKey,void*,int losingFocus){ContextScope contextScope;auto& input=ImGui::GetIO();if(losingFocus){input.AddFocusEvent(false);return;}input.AddFocusEvent(true);bool down=(flags&xplm_UpFlag)==0;input.AddKeyEvent(ImGuiMod_Shift,(flags&xplm_ShiftFlag)!=0);input.AddKeyEvent(ImGuiMod_Ctrl,(flags&xplm_ControlFlag)!=0);input.AddKeyEvent(ImGuiMod_Alt,(flags&xplm_OptionAltFlag)!=0);
    ImGuiKey key=ImGuiKey_None;switch(static_cast<unsigned char>(virtualKey)){case 8:key=ImGuiKey_Backspace;break;case 9:key=ImGuiKey_Tab;break;case 13:key=ImGuiKey_Enter;break;case 27:key=ImGuiKey_Escape;break;case 37:key=ImGuiKey_LeftArrow;break;case 38:key=ImGuiKey_UpArrow;break;case 39:key=ImGuiKey_RightArrow;break;case 40:key=ImGuiKey_DownArrow;break;case 46:key=ImGuiKey_Delete;break;case 36:key=ImGuiKey_Home;break;case 35:key=ImGuiKey_End;break;default:if(virtualKey>='A'&&virtualKey<='Z')key=static_cast<ImGuiKey>(ImGuiKey_A+virtualKey-'A');}
    if(key!=ImGuiKey_None)input.AddKeyEvent(key,down);if(down && static_cast<unsigned char>(character)>=32 && !(flags&xplm_ControlFlag))input.AddInputCharacter(static_cast<unsigned char>(character));if(down&&key==ImGuiKey_Escape)XPLMTakeKeyboardFocus(nullptr);
}
float flightLoop(float,float,int,void*){if(!enabled||!engine)return 0.5f;ensureEngineRunning();openatc::Telemetry telemetry;telemetry.latitude=XPLMGetDatad(latitudeRef);telemetry.longitude=XPLMGetDatad(longitudeRef);telemetry.altitudeFeet=XPLMGetDatad(altitudeRef)*3.280839895;telemetry.groundSpeedKnots=XPLMGetDataf(speedRef)*1.943844492;telemetry.headingDegrees=XPLMGetDataf(headingRef);telemetry.onGround=XPLMGetDatai(groundRef)!=0;telemetry.paused=XPLMGetDatai(pauseRef)!=0;telemetry.verticalSpeedFpm=verticalSpeedRef?XPLMGetDataf(verticalSpeedRef):0;telemetry.heightAglFeet=aglRef?XPLMGetDataf(aglRef)*3.280839895:0;telemetry.com1Khz=com1Ref?XPLMGetDatai(com1Ref):0;telemetry.positionValid=true;
    if(interface){ContextScope contextScope;interface->tick();}
    auto snapshot=engine->state();auto settings=engine->settings();
    if(snapshot.nextSequence<lastStateSequence)lastFrequencySequence=0;lastStateSequence=snapshot.nextSequence;
    if(settings.copilotTunes && snapshot.frequencySequence>lastFrequencySequence && snapshot.recommendedFrequencyKhz>=118000 && snapshot.recommendedFrequencyKhz<=136990 && com1Ref && XPLMCanWriteDataRef(com1Ref)) {
        XPLMSetDatai(com1Ref,snapshot.recommendedFrequencyKhz);lastFrequencySequence=snapshot.frequencySequence;
    }
    engine->telemetry(telemetry);return 0.5f;}
int toggle(XPLMCommandRef,XPLMCommandPhase phase,void*){if(phase==xplm_CommandBegin&&windowId&&enabled)XPLMSetWindowIsVisible(windowId,!XPLMGetWindowIsVisible(windowId));return 1;}
void menu(void*,void*){toggle(nullptr,xplm_CommandBegin,nullptr);}
}
PLUGIN_API int XPluginStart(char* name,char* signature,char* description){std::strcpy(name,openatc::productName);std::strcpy(signature,"org.openatc.development");std::strcpy(description,"OpenATC AI development UI and simulator controller interface");XPLMEnableFeature("XPLM_USE_NATIVE_PATHS",1);
    auto* previousContext=ImGui::GetCurrentContext();context=ImGui::CreateContext();ImGui::SetCurrentContext(previousContext);ContextScope contextScope;ImGui::GetIO().IniFilename=nullptr;ImGui::GetIO().BackendFlags|=ImGuiBackendFlags_RendererHasVtxOffset;
#if LIN
    ImGui::GetIO().SetClipboardTextFn=setClipboardText;ImGui::GetIO().GetClipboardTextFn=getClipboardText;
#endif
    openatc::Interface::configureStyle();openatc::Interface::configureFonts();
    XPLMCreateWindow_t parameters{};parameters.structSize=sizeof(parameters);parameters.left=60;parameters.top=940;parameters.right=1260;parameters.bottom=120;parameters.visible=0;parameters.drawWindowFunc=drawWindow;parameters.handleMouseClickFunc=mouseClick;parameters.handleKeyFunc=keyboard;parameters.handleCursorFunc=cursor;parameters.handleMouseWheelFunc=mouseWheel;parameters.handleRightClickFunc=rightClick;parameters.decorateAsFloatingWindow=xplm_WindowDecorationRoundRectangle;parameters.layer=xplm_WindowLayerFloatingWindows;windowId=XPLMCreateWindowEx(&parameters);if(!windowId){ImGui::DestroyContext(context);context=nullptr;return 0;}XPLMSetWindowTitle(windowId,openatc::productName);XPLMSetWindowResizingLimits(windowId,1060,760,2200,1600);
    toggleCommand=XPLMCreateCommand("openatc/toggle_window","Toggle OpenATC AI window");XPLMRegisterCommandHandler(toggleCommand,toggle,1,nullptr);menuIndex=XPLMAppendMenuItem(XPLMFindPluginsMenu(),openatc::productName,nullptr,0);menuId=XPLMCreateMenu(openatc::productName,XPLMFindPluginsMenu(),menuIndex,menu,nullptr);XPLMAppendMenuItem(menuId,"Show / hide",nullptr,0);return 1;
}
PLUGIN_API int XPluginEnable(){latitudeRef=XPLMFindDataRef("sim/flightmodel/position/latitude");longitudeRef=XPLMFindDataRef("sim/flightmodel/position/longitude");altitudeRef=XPLMFindDataRef("sim/flightmodel/position/elevation");speedRef=XPLMFindDataRef("sim/flightmodel/position/groundspeed");headingRef=XPLMFindDataRef("sim/flightmodel/position/psi");groundRef=XPLMFindDataRef("sim/flightmodel/failures/onground_any");pauseRef=XPLMFindDataRef("sim/time/paused");verticalSpeedRef=XPLMFindDataRef("sim/flightmodel/position/vh_ind_fpm");aglRef=XPLMFindDataRef("sim/flightmodel/position/y_agl");com1Ref=XPLMFindDataRef("sim/cockpit2/radios/actuators/com1_frequency_hz_833");if(!latitudeRef||!longitudeRef||!altitudeRef||!speedRef||!headingRef||!groundRef||!pauseRef)return 0;engine=std::make_unique<openatc::EngineClient>();interface=std::make_unique<openatc::Interface>(*engine);char simulatorPath[2048]{};XPLMGetSystemPath(simulatorPath);engine->post("/simulator/root",{{"root",simulatorPath}});lastFrequencySequence=0;enabled=true;engineBinaryPath.clear();disconnectedTiming=false;spawnFailures=0;binaryMissingLogged=false;lastSpawnAttempt={};XPLMRegisterFlightLoopCallback(flightLoop,0.5f,nullptr);return 1;}
PLUGIN_API void XPluginDisable(){enabled=false;XPLMUnregisterFlightLoopCallback(flightLoop,nullptr);XPLMSetWindowIsVisible(windowId,0);XPLMTakeKeyboardFocus(nullptr);interface.reset();engine.reset();}
PLUGIN_API void XPluginStop(){if(enabled)XPluginDisable();if(toggleCommand)XPLMUnregisterCommandHandler(toggleCommand,toggle,1,nullptr);if(menuId)XPLMDestroyMenu(menuId);if(menuIndex>=0)XPLMRemoveMenuItem(XPLMFindPluginsMenu(),menuIndex);if(windowId)XPLMDestroyWindow(windowId);if(context)ImGui::DestroyContext(context);fontTexture=0;context=nullptr;windowId=nullptr;
#if LIN
    if(graphicsLibrary)dlclose(graphicsLibrary);graphicsLibrary=nullptr;graphicsReady=false;
#endif
}
PLUGIN_API void XPluginReceiveMessage(XPLMPluginID,int,void*){}
