// Minimal stub library to prevent EGL "couldn't find an OpenGL ES implementation" abort
// dlopen succeeds, dlsym returns NULL for EGL functions (handled gracefully by loader)
__attribute__((visibility("default")))
int eglGetDisplay(void) { return 0; }

__attribute__((visibility("default")))
int eglInitialize(void) { return 0; }

__attribute__((visibility("default")))
int eglTerminate(void) { return 0; }
