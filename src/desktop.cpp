#include "openatc/branding.hpp"
#include "openatc/ui.hpp"
#if defined(__linux__)
#define GLFW_INCLUDE_NONE
#include <glad/gl.h>
#endif
#include <GLFW/glfw3.h>
#include <imgui_impl_glfw.h>
#include <imgui_impl_opengl2.h>
#include <iostream>
#include <cstdlib>

int main(int argumentCount,char** arguments) {
    #if defined(__linux__)
    glfwInitHint(GLFW_PLATFORM, GLFW_PLATFORM_WAYLAND);
#endif
    if(!glfwInit()){std::cerr<<"GLFW initialization failed\n";return 1;}
    #if defined(__linux__)
    glfwWindowHint(GLFW_CONTEXT_CREATION_API, GLFW_EGL_CONTEXT_API);
#endif
    GLFWwindow* window=glfwCreateWindow(1280,860,openatc::productName,nullptr,nullptr);
    if(!window){glfwTerminate();return 1;}glfwMakeContextCurrent(window);
#if defined(__linux__)
    if(!gladLoadGL(reinterpret_cast<GLADloadfunc>(glfwGetProcAddress))){std::cerr<<"Cannot load OpenGL functions through Wayland/EGL\n";glfwDestroyWindow(window);glfwTerminate();return 1;}
#endif
    glfwSwapInterval(1);
    IMGUI_CHECKVERSION();ImGui::CreateContext();ImGui::GetIO().IniFilename=nullptr;
    openatc::Interface::configureFonts();
    openatc::Interface::configureStyle();ImGui_ImplGlfw_InitForOpenGL(window,true);ImGui_ImplOpenGL2_Init();
    {openatc::EngineClient engine(argumentCount>1?arguments[1]:"http://127.0.0.1:8087");openatc::Interface interface(engine);
        while(!glfwWindowShouldClose(window)){glfwPollEvents();ImGui_ImplOpenGL2_NewFrame();ImGui_ImplGlfw_NewFrame();ImGui::NewFrame();interface.draw({0,0},ImGui::GetIO().DisplaySize);ImGui::Render();int width,height;glfwGetFramebufferSize(window,&width,&height);glViewport(0,0,width,height);glClearColor(0.035f,0.055f,0.075f,1);glClear(GL_COLOR_BUFFER_BIT);ImGui_ImplOpenGL2_RenderDrawData(ImGui::GetDrawData());glfwSwapBuffers(window);}
    }
    ImGui_ImplOpenGL2_Shutdown();ImGui_ImplGlfw_Shutdown();ImGui::DestroyContext();glfwDestroyWindow(window);glfwTerminate();
}
