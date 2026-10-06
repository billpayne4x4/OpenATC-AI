#define GLFW_INCLUDE_NONE
#include <GLFW/glfw3.h>
#include <imgui.h>
#include <imgui_impl_glfw.h>
#include <algorithm>

namespace {
GLFWwindow* applicationWindow=nullptr;
double previousTime=0;
ImGuiKey translateKey(int key){
    if(key>=GLFW_KEY_A&&key<=GLFW_KEY_Z)return static_cast<ImGuiKey>(ImGuiKey_A+key-GLFW_KEY_A);
    if(key>=GLFW_KEY_0&&key<=GLFW_KEY_9)return static_cast<ImGuiKey>(ImGuiKey_0+key-GLFW_KEY_0);
    if(key>=GLFW_KEY_F1&&key<=GLFW_KEY_F12)return static_cast<ImGuiKey>(ImGuiKey_F1+key-GLFW_KEY_F1);
    switch(key){
    case GLFW_KEY_TAB:return ImGuiKey_Tab;
    case GLFW_KEY_LEFT:return ImGuiKey_LeftArrow;
    case GLFW_KEY_RIGHT:return ImGuiKey_RightArrow;
    case GLFW_KEY_UP:return ImGuiKey_UpArrow;
    case GLFW_KEY_DOWN:return ImGuiKey_DownArrow;
    case GLFW_KEY_PAGE_UP:return ImGuiKey_PageUp;
    case GLFW_KEY_PAGE_DOWN:return ImGuiKey_PageDown;
    case GLFW_KEY_HOME:return ImGuiKey_Home;
    case GLFW_KEY_END:return ImGuiKey_End;
    case GLFW_KEY_INSERT:return ImGuiKey_Insert;
    case GLFW_KEY_DELETE:return ImGuiKey_Delete;
    case GLFW_KEY_BACKSPACE:return ImGuiKey_Backspace;
    case GLFW_KEY_SPACE:return ImGuiKey_Space;
    case GLFW_KEY_ENTER:return ImGuiKey_Enter;
    case GLFW_KEY_ESCAPE:return ImGuiKey_Escape;
    case GLFW_KEY_LEFT_SHIFT:return ImGuiKey_LeftShift;
    case GLFW_KEY_RIGHT_SHIFT:return ImGuiKey_RightShift;
    case GLFW_KEY_LEFT_CONTROL:return ImGuiKey_LeftCtrl;
    case GLFW_KEY_RIGHT_CONTROL:return ImGuiKey_RightCtrl;
    case GLFW_KEY_LEFT_ALT:return ImGuiKey_LeftAlt;
    case GLFW_KEY_RIGHT_ALT:return ImGuiKey_RightAlt;
    case GLFW_KEY_LEFT_SUPER:return ImGuiKey_LeftSuper;
    case GLFW_KEY_RIGHT_SUPER:return ImGuiKey_RightSuper;
    default:return ImGuiKey_None;
    }
}
}
bool ImGui_ImplGlfw_InitForOpenGL(GLFWwindow* window,bool installCallbacks){
    if(glfwGetPlatform()!=GLFW_PLATFORM_WAYLAND)return false;
    applicationWindow=window;previousTime=glfwGetTime();auto& input=ImGui::GetIO();input.BackendPlatformName="openatc_wayland";
    if(installCallbacks){
        glfwSetCursorPosCallback(window,[](GLFWwindow*,double horizontal,double vertical){ImGui::GetIO().AddMousePosEvent(static_cast<float>(horizontal),static_cast<float>(vertical));});
        glfwSetCursorEnterCallback(window,[](GLFWwindow*,int entered){if(!entered)ImGui::GetIO().AddMousePosEvent(-3.4e38f,-3.4e38f);});
        glfwSetMouseButtonCallback(window,[](GLFWwindow*,int button,int action,int){if(button>=0&&button<5)ImGui::GetIO().AddMouseButtonEvent(button,action==GLFW_PRESS);});
        glfwSetScrollCallback(window,[](GLFWwindow*,double horizontal,double vertical){ImGui::GetIO().AddMouseWheelEvent(static_cast<float>(horizontal),static_cast<float>(vertical));});
        glfwSetCharCallback(window,[](GLFWwindow*,unsigned character){ImGui::GetIO().AddInputCharacter(character);});
        glfwSetWindowFocusCallback(window,[](GLFWwindow*,int focused){ImGui::GetIO().AddFocusEvent(focused!=0);});
        glfwSetKeyCallback(window,[](GLFWwindow*,int key,int,int action,int modifiers){auto& input=ImGui::GetIO();input.AddKeyEvent(ImGuiMod_Ctrl,(modifiers&GLFW_MOD_CONTROL)!=0);input.AddKeyEvent(ImGuiMod_Shift,(modifiers&GLFW_MOD_SHIFT)!=0);input.AddKeyEvent(ImGuiMod_Alt,(modifiers&GLFW_MOD_ALT)!=0);input.AddKeyEvent(ImGuiMod_Super,(modifiers&GLFW_MOD_SUPER)!=0);auto translated=translateKey(key);if(translated!=ImGuiKey_None)input.AddKeyEvent(translated,action!=GLFW_RELEASE);});
    }
    input.GetClipboardTextFn=[](void* user){return glfwGetClipboardString(static_cast<GLFWwindow*>(user));};
    input.SetClipboardTextFn=[](void* user,const char* text){glfwSetClipboardString(static_cast<GLFWwindow*>(user),text);};
    input.ClipboardUserData=window;
    return true;
}
void ImGui_ImplGlfw_NewFrame(){
    int width,height,framebufferWidth,framebufferHeight;glfwGetWindowSize(applicationWindow,&width,&height);glfwGetFramebufferSize(applicationWindow,&framebufferWidth,&framebufferHeight);auto& input=ImGui::GetIO();input.DisplaySize={static_cast<float>(width),static_cast<float>(height)};if(width>0&&height>0)input.DisplayFramebufferScale={static_cast<float>(framebufferWidth)/width,static_cast<float>(framebufferHeight)/height};double now=glfwGetTime();input.DeltaTime=static_cast<float>(std::max(0.0001,now-previousTime));previousTime=now;
}
void ImGui_ImplGlfw_Shutdown(){auto& input=ImGui::GetIO();input.BackendPlatformName=nullptr;input.GetClipboardTextFn=nullptr;input.SetClipboardTextFn=nullptr;input.ClipboardUserData=nullptr;applicationWindow=nullptr;}
