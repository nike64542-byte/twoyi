// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use jni::objects::JValue;
use jni::sys::{jclass, jfloat, jint, jintArray, jobject, jfloatArray, JNI_ERR, jstring};
use jni::JNIEnv;
use jni::{JavaVM, NativeMethod};
use log::{error, info, Level, debug};
use ndk_sys;
use std::ffi::c_void;

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use android_logger::Config;

use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

mod input;
mod renderer_bindings;

/// ## Examples
/// ```
/// let method:NativeMethod = jni_method!(native_method, "(Ljava/lang/String;)V");
/// ```
macro_rules! jni_method {
    ( $name: tt, $method:tt, $signature:expr ) => {{
        jni::NativeMethod {
            name: jni::strings::JNIString::from(stringify!($name)),
            sig: jni::strings::JNIString::from($signature),
            fn_ptr: $method as *mut c_void,
        }
    }};
}

static RENDERER_STARTED: AtomicBool = AtomicBool::new(false);

#[no_mangle]
pub fn renderer_init(
    env: JNIEnv,
    _clz: jclass,
    surface: jobject,
    loader: jstring,
    xdpi: jfloat,
    ydpi: jfloat,
    fps: jint,
) {
    debug!("renderer_init");
    let window = unsafe { ndk_sys::ANativeWindow_fromSurface(env.get_native_interface(), surface) };

    let window = match std::ptr::NonNull::new(window) {
        Some(x) => x,
        None => {
            error!("ANativeWindow_fromSurface was null!");
            return;
        }
    };

    let window = unsafe { ndk::native_window::NativeWindow::from_ptr(window) };

    let width = window.width();
    let height = window.height();

    info!(
        "renderer_init width: {}, height: {}, fps: {}",
        width, height, fps
    );

    if RENDERER_STARTED.compare_exchange(false, true, 
        Ordering::Acquire, Ordering::Relaxed).is_err() {
        let win = window.ptr().as_ptr() as *mut c_void;
        unsafe {
            renderer_bindings::setNativeWindow(win);
            renderer_bindings::resetSubWindow(win, 0, 0, width, height, width, height, 1.0, 0.0);
        }
    } else {
        input::start_input_system(width, height);

        thread::spawn(move || {
            let win = window.ptr().as_ptr() as *mut c_void;
            info!("win: {:#?}", win);
            unsafe {
                renderer_bindings::startOpenGLRenderer(
                    win,
                    width,
                    height,
                    xdpi as i32,
                    ydpi as i32,
                    fps as i32,
                );
            }
        });

        let loader_path: String = match env.get_string(loader.into()) {
            Ok(s) => s.into(),
            Err(e) => {
                error!("Failed to get loader path: {:?}", e);
                return;
            }
        };
        let working_dir = "/data/data/io.twoyi/rootfs";
        let log_path = "/data/data/io.twoyi/log.txt";

        // Check if init binary exists and collect diagnostic info
        let init_path = format!("{}/init", working_dir);
        let mut diag = String::new();
        diag.push_str(&format!("=== twoyi boot diagnostic ===\n"));
        diag.push_str(&format!("working_dir={}\n", working_dir));
        diag.push_str(&format!("loader_path={}\n", loader_path));
        diag.push_str(&format!("log_path={}\n", log_path));
        diag.push_str(&format!("init_path={}\n", init_path));

        // Check loader64 symlink
        let loader64_path = "/data/data/io.twoyi/loader64";
        match std::fs::read_link(loader64_path) {
            Ok(target) => diag.push_str(&format!("loader64 symlink -> {}\n", target.display())),
            Err(e) => diag.push_str(&format!("loader64 symlink error: {:?}\n", e)),
        }

        // Check init binary
        match std::fs::metadata(&init_path) {
            Ok(meta) => {
                let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions());
                diag.push_str(&format!("init found: size={}, mode={:#o}, executable={}\n", meta.len(), mode, mode & 0o111 != 0));
                
                // Read first bytes to check ELF magic
                if let Ok(mut f) = std::fs::File::open(&init_path) {
                    use std::io::Read;
                    let mut buf = [0u8; 32];
                    if let Ok(n) = f.read(&mut buf) {
                        diag.push_str(&format!("init first {} bytes: {:?}\n", n, &buf[..n]));
                        if n >= 4 && &buf[..4] == b"\x7fELF" {
                            diag.push_str("init is ELF binary\n");
                            if n >= 5 {
                                diag.push_str(&format!("ELF class: {} (1=32bit, 2=64bit)\n", buf[4]));
                            }
                        } else if n >= 2 && &buf[..2] == b"#!" {
                            diag.push_str("init is a script\n");
                        }
                    }
                }
            }
            Err(e) => {
                diag.push_str(&format!("init NOT found: {:?}\n", e));
                // List rootfs dir contents
                if let Ok(entries) = std::fs::read_dir(working_dir) {
                    diag.push_str("rootfs dir contents:\n");
                    for entry in entries.flatten().take(50) {
                        let name = entry.file_name();
                        diag.push_str(&format!("  {}\n", name.to_string_lossy()));
                    }
                }
            }
        }

        // Check loader64 binary
        match std::fs::metadata(loader64_path) {
            Ok(meta) => {
                let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions());
                diag.push_str(&format!("loader64 found: size={}, mode={:#o}, executable={}\n", meta.len(), mode, mode & 0o111 != 0));
            }
            Err(e) => {
                diag.push_str(&format!("loader64 NOT found: {:?}\n", e));
            }
        }

        let outputs = match File::create(log_path) {
            Ok(f) => f,
            Err(e) => {
                error!("Failed to create log file: {:?}", e);
                return;
            }
        };

        // Write diagnostic info to log file before init starts
        {
            use std::io::Write;
            let mut log_writer = std::io::BufWriter::new(&outputs);
            let _ = writeln!(log_writer, "{}", diag);
            let _ = log_writer.flush();
        }

        let errors = match outputs.try_clone() {
            Ok(f) => f,
            Err(e) => {
                error!("Failed to clone log file: {:?}", e);
                return;
            }
        };
        match Command::new("./init")
            .current_dir(working_dir)
            .env("TYLOADER", loader_path)
            .stdout(Stdio::from(outputs))
            .stderr(Stdio::from(errors))
            .spawn() {
            Ok(child) => {
                info!("init spawned successfully, pid={}", child.id());
                // Also write to log file
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(log_path) {
                    use std::io::Write;
                    let _ = writeln!(f, "init spawned: pid={}", child.id());
                }
            }
            Err(e) => {
                error!("init spawn FAILED: {:?}", e);
                // Write error to log file
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(log_path) {
                    use std::io::Write;
                    let _ = writeln!(f, "init spawn FAILED: {:?}", e);
                }
            }
        }
    }
}

#[no_mangle]
pub fn renderer_reset_window(
    env: JNIEnv,
    _clz: jclass,
    surface: jobject,
    _top: jint,
    _left: jint,
    _width: jint,
    _height: jint,
) {
    debug!("reset_window");
    unsafe {
        let window = ndk_sys::ANativeWindow_fromSurface(env.get_native_interface(), surface);
        renderer_bindings::resetSubWindow(window as *mut c_void, 0, 0, _width, _height, _width, _height, 1.0, 0.0);
    }
}

#[no_mangle]
pub fn renderer_remove_window(env: JNIEnv, _clz: jclass, surface: jobject) {
    debug!("renderer_remove_window");

    unsafe {
        let window = ndk_sys::ANativeWindow_fromSurface(env.get_native_interface(), surface);
        renderer_bindings::removeSubWindow(window as *mut c_void);
    }
}

#[no_mangle]
#[deprecated(since = "0.5.5", note = "Use handle_touch_data instead for Android 16+ compatibility")]
pub fn handle_touch(env: JNIEnv, _clz: jclass, event: jobject) {
    let ptr = match env.get_field(event, "mNativePtr", "J") {
        Ok(v) => v,
        Err(e) => {
            error!("handle_touch: failed to get mNativePtr: {:?}", e);
            return;
        }
    };

    if let JValue::Long(p) = ptr {
        let ev = unsafe {
            let nonptr = match std::ptr::NonNull::new(std::mem::transmute::<i64, *mut ndk_sys::AInputEvent>(p)) {
                Some(ptr) => ptr,
                None => {
                    error!("handle_touch: null native pointer");
                    return;
                }
            };
            ndk::event::MotionEvent::from_ptr(nonptr)
        };
        input::handle_touch(ev)
    }
}

const MAX_POINTERS: usize = 5;

#[no_mangle]
pub fn handle_touch_data(
    env: JNIEnv, _clz: jclass,
    action: jint,
    pointer_index: jint,
    pointer_count: jint,
    pointer_ids: jintArray,
    xs: jfloatArray,
    ys: jfloatArray,
    pressures: jfloatArray,
) {
    let action = action as i32;
    let pointer_index = pointer_index as usize;
    let pointer_count = pointer_count as usize;

    if pointer_count > MAX_POINTERS {
        error!("pointer_count {} exceeds MAX_POINTERS {}", pointer_count, MAX_POINTERS);
        return;
    }

    // Use stack-allocated arrays to avoid heap allocation on each touch event
    let mut ids_buf = [0i32; MAX_POINTERS];
    let mut x_buf = [0f32; MAX_POINTERS];
    let mut y_buf = [0f32; MAX_POINTERS];
    let mut p_buf = [0f32; MAX_POINTERS];

    if let Err(e) = env.get_int_array_region(pointer_ids, 0, &mut ids_buf[..pointer_count]) {
        error!("Failed to read pointer IDs array: {:?}", e);
        return;
    }
    if let Err(e) = env.get_float_array_region(xs, 0, &mut x_buf[..pointer_count]) {
        error!("Failed to read X coordinates array: {:?}", e);
        return;
    }
    if let Err(e) = env.get_float_array_region(ys, 0, &mut y_buf[..pointer_count]) {
        error!("Failed to read Y coordinates array: {:?}", e);
        return;
    }
    if let Err(e) = env.get_float_array_region(pressures, 0, &mut p_buf[..pointer_count]) {
        error!("Failed to read pressures array: {:?}", e);
        return;
    }

    input::handle_touch_data(action, pointer_index, pointer_count, &ids_buf[..pointer_count], &x_buf[..pointer_count], &y_buf[..pointer_count], &p_buf[..pointer_count]);
}

pub fn send_key_code(_env: JNIEnv, _clz: jclass, keycode: jint) {
    debug!("send key code!");
    input::send_key_code(keycode);
}

unsafe fn register_natives(jvm: &JavaVM, class_name: &str, methods: &[NativeMethod]) -> jint {
    let env: JNIEnv = jvm.get_env().unwrap();
    let jni_version = env.get_version().unwrap();
    let version: jint = jni_version.into();

    debug!("JNI Version : {:#?} ", jni_version);

    let clazz = match env.find_class(class_name) {
        Ok(clazz) => clazz,
        Err(e) => {
            error!("java class not found : {:?}", e);
            return JNI_ERR;
        }
    };
    debug!("clazz: {:#?}", clazz);

    let result = env.register_native_methods(clazz, &methods);

    if result.is_ok() {
        debug!("register_natives : succeed");
        version
    } else {
        error!("register_natives : failed ");
        JNI_ERR
    }
}

#[no_mangle]
#[allow(non_snake_case)]
unsafe fn JNI_OnLoad(jvm: JavaVM, _reserved: *mut c_void) -> jint {
    android_logger::init_once(
        Config::default()
            .with_min_level(Level::Info)
            .with_tag("CLIENT_EGL"),
    );

    debug!("JNI_OnLoad");

    let class_name: &str = "io/twoyi/Renderer";
    let jni_methods = [
        jni_method!(init, renderer_init, "(Landroid/view/Surface;Ljava/lang/String;FFI)V"),
        jni_method!(
            resetWindow,
            renderer_reset_window,
            "(Landroid/view/Surface;IIII)V"
        ),
        jni_method!(
            removeWindow,
            renderer_remove_window,
            "(Landroid/view/Surface;)V"
        ),
        jni_method!(handleTouch, handle_touch, "(Landroid/view/MotionEvent;)V"),
        jni_method!(handleTouchData, handle_touch_data, "(III[I[F[F[F)V"),
        jni_method!(sendKeycode, send_key_code, "(I)V"),
    ];

    register_natives(&jvm, class_name, jni_methods.as_ref())
}
