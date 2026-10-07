//! JNI entry points, so Kotlin can drive the kernel without a C shim of its own.
//!
//! The C ABI in `lib.rs` takes the protector as a bare function pointer, which is
//! the right shape for a C caller and impossible to build from Kotlin: a Kotlin
//! lambda is an object, not an address. So this module keeps a global reference to
//! the Kotlin object and calls its `protect(int): boolean` from the engine's
//! thread.
//!
//! # The thread problem
//!
//! `Protector::protect` is called from whichever thread the engine is running on,
//! and that thread was created by Rust — the JVM has never seen it. A JNI call
//! from an unknown thread is undefined behaviour, so every call attaches first
//! and detaches after. `AttachCurrentThread` on an already-attached thread is
//! cheap and correct, which is why it is done unconditionally rather than
//! tracked.
//!
//! Enabled by the `jni-bridge` feature. A plain host build leaves it out and the
//! C ABI stays testable without a JVM.

use std::ffi::CString;
use std::os::unix::io::RawFd;

use jni::objects::{GlobalRef, JByteArray, JClass, JLongArray, JObject, JString};
use jni::sys::{jdouble, jint, jlong, jstring};
use jni::{JNIEnv, JavaVM};

use crate::WattEngine;

/// Everything the callback needs, held for the engine's lifetime.
struct JvmProtector {
    vm: JavaVM,
    target: GlobalRef,
}

impl JvmProtector {
    /// Call `protect(fd)` on the Kotlin object.
    ///
    /// A JNI failure here answers `false`, which the engine treats as a failed
    /// protection and abandons the connection. That is the safe direction: the
    /// alternative is connecting anyway and handing the socket back to the
    /// tunnel, which is the loop this whole mechanism exists to prevent.
    fn protect(&self, fd: RawFd) -> bool {
        // `AttachCurrentThread` on an attached thread returns the existing env,
        // so there is nothing to check first.
        let mut env = match self.vm.attach_current_thread() {
            Ok(env) => env,
            Err(err) => {
                log(&format!("could not attach to the JVM: {err}"));
                return false;
            }
        };

        let result = (|| -> jni::errors::Result<bool> {
            let value = env.call_method(
                self.target.as_obj(),
                "protect",
                "(I)Z",
                &[jni::objects::JValue::Int(fd)],
            )?;
            value.z()
        })();

        match result {
            Ok(protected) => protected,
            Err(err) => {
                log(&format!("the protector threw: {err}"));
                false
            }
        }
    }
}

/// Write to logcat under a fixed tag.
///
/// `android.util.Log` is not reachable from here without another dependency, and
/// a failure inside the protector is exactly the kind of thing that would
/// otherwise vanish silently.
fn log(message: &str) {
    eprintln!("DetourKernel: {message}");
}

/// Route the kernel's own diagnostics to logcat.
///
/// # Why this is needed at all
///
/// The kernel reports through `eprintln!` and `log`. On Android the first writes
/// to the C `stderr` inherited from the JVM, which `adb logcat` does not surface
/// under a readable tag — in practice the proxy's `proxy: ...` lines were simply
/// invisible, and every conclusion of the form "the kernel logged nothing" was
/// drawn blind. The second needs a logger installed before the first call.
///
/// # Why it is called from everywhere
///
/// The proxy path never goes through `nativeNew`: `nativeProxyStart` is called on
/// its own, so a `Once` inside `nativeNew` would leave proxy mode with no logger
/// at all. This is idempotent, so calling it from every entry point costs one
/// atomic load after the first.
fn init_logcat() {
    #[cfg(all(target_os = "android", feature = "logcat"))]
    {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            // Debug level, so the per-address probe lines can be turned on by
            // reading the log at that level rather than by rebuilding. The
            // default `logcat` filter shows everything at or above the level a
            // call is made with; the chatty diagnostics use `debug!` and the
            // things worth seeing on every request use `info!`.
            android_logger::init_once(
                android_logger::Config::default()
                    .with_tag("DetourKernel")
                    .with_max_level(log::LevelFilter::Debug),
            );
        });
    }
}

/// Build an engine.
///
/// `config_json` carries the user's kernel settings. It is JSON rather than a
/// positional struct so that adding a knob is a change in one place instead of
/// three, and so an unknown key can be ignored rather than shifting every field
/// after it. An empty or unparsable document falls back to the defaults, which
/// are the kernel's own — a bad setting should not stop the tunnel.
///
/// `protect` is required by the Kotlin side; passing null here still gets an
/// explicit refusal rather than an engine that will relay into itself.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeNew(
    mut env: JNIEnv,
    _class: JClass,
    tun_fd: jint,
    tun_name: JString,
    rules: JByteArray,
    protect: JObject,
    config_json: JString,
) -> jlong {
    if protect.is_null() {
        set_error(&mut env, "no protector: the shell must supply VpnService.protect");
        return 0;
    }

    // Before anything else can fail: without this a failure below is invisible.
    init_logcat();

    let vm = match env.get_java_vm() {
        Ok(vm) => vm,
        Err(err) => {
            set_error(&mut env, &format!("no JavaVM: {err}"));
            return 0;
        }
    };
    let target = match env.new_global_ref(&protect) {
        Ok(target) => target,
        Err(err) => {
            set_error(&mut env, &format!("could not hold the protector: {err}"));
            return 0;
        }
    };

    let document = match env.convert_byte_array(&rules) {
        Ok(document) => document,
        Err(err) => {
            set_error(&mut env, &format!("reading the rule document: {err}"));
            return 0;
        }
    };
    let name = if tun_name.is_null() {
        None
    } else {
        env.get_string(&tun_name).ok().map(|s| s.into())
    };
    let config = if config_json.is_null() {
        String::new()
    } else {
        env.get_string(&config_json)
            .map(|s| s.into())
            .unwrap_or_default()
    };

    let protector = JvmProtector { vm, target };
    let handler = Box::new(move |fd: RawFd| protector.protect(fd));

    match crate::build_engine_with_config(tun_fd, name, &document, &config, handler) {
        Ok(engine) => Box::into_raw(engine) as jlong,
        Err(message) => {
            set_error(&mut env, &message);
            0
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeStep(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    let Some(claim) = (unsafe { claim_engine(handle) }) else {
        return -1;
    };
    // The claim is what makes this borrow sound: `free` cannot start until the
    // claim drops, and it cannot have started before, because the claim would
    // not have been granted. The `&mut` is reconstructed from the same handle
    // the claim came from, and no other thread can be inside this engine — the
    // shell drives `step` from exactly one run loop.
    let engine = unsafe { &mut *(handle as *mut WattEngine) };
    let result = crate::step(engine) as jint;
    drop(claim);
    result
}

#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeReplaceRules(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    rules: JByteArray,
) -> jint {
    let Some(claim) = (unsafe { claim_engine(handle) }) else {
        set_error(&mut env, "the engine is not running");
        return -1;
    };
    let engine = unsafe { &mut *(handle as *mut WattEngine) };
    let result = match env.convert_byte_array(&rules) {
        Ok(document) => {
            if crate::replace_rules(engine, &document) {
                0
            } else {
                set_error(&mut env, "the kernel rejected the rule document");
                -1
            }
        }
        Err(err) => {
            set_error(&mut env, &format!("reading the rule document: {err}"));
            -1
        }
    };
    drop(claim);
    result
}

/// Fill `out` with the counters, in the order `Kernel.Stats` reads them.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeStats(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    out: JLongArray,
) -> jint {
    let Some(engine) = (unsafe { claim_engine(handle) }) else {
        set_error(&mut env, "the engine is not running");
        return -1;
    };
    let Some(stats) = crate::collect_stats(engine.engine()) else {
        set_error(&mut env, "the kernel returned no counters");
        return -1;
    };

    let values = [
        stats.packets_in,
        stats.packets_out,
        stats.tcp_flows_opened,
        stats.tcp_flows_closed,
        stats.tcp_flows_rejected,
        stats.tcp_connect_failures,
        stats.udp_flows_opened,
        stats.udp_flows_evicted,
        stats.dns_queries,
        stats.dns_answered_locally,
        stats.dns_trimmed,
        stats.bytes_client_to_upstream,
        stats.bytes_upstream_to_client,
        stats.live_flows,
        stats.flows_matched_rules,
        stats.flows_direct,
        stats.flows_without_name,
        stats.flows_named_by_sni,
        // Appended, never inserted: `Kernel.Stats` reads these by index, so a new
        // counter goes on the end and the two files' orders stay one list.
        stats.proxy_handshakes,
        stats.proxy_refusals,
        stats.dns_upstream_queries,
        stats.dns_upstream_answered,
        stats.dns_upstream_failed,
        stats.dns_upstream_retries,
        stats.dns_upstream_overflowed,
        stats.flows_named_without_move,
        stats.flows_named_under_ech,
        stats.hellos_without_name,
    ];

    let widened: Vec<jlong> = values.iter().map(|v| *v as jlong).collect();
    if env.set_long_array_region(&out, 0, &widened).is_err() {
        set_error(&mut env, "could not write the counters");
        return -1;
    }
    0
}

#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeUptime(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jdouble {
    let Some(engine) = (unsafe { claim_engine(handle) }) else {
        return 0.0;
    };
    crate::uptime_seconds(engine.engine())
}

/// The per-address history and certificate verdicts, as a JSON string.
///
/// Returned as a `jstring` rather than through the C ABI's `char *` so there is
/// no buffer for the shell to free: the JVM copies the bytes when the string is
/// built, and the Rust `String` drops on return. The C entry point
/// (`watt_engine_ip_stats`) keeps the `watt_free_buffer` contract for non-Java
/// callers; both render through `ip_stats_json`, so the two surfaces cannot
/// disagree about the document's shape.
///
/// Null means the handle was already freed. The shell shows "no data" rather
/// than an error for that: the rules screen can be open while the tunnel is
/// being torn down, and a snapshot taken during teardown is not a failure worth
/// a message.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeIpStats(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    let Some(engine) = (unsafe { claim_engine(handle) }) else {
        return std::ptr::null_mut();
    };
    match env.new_string(crate::ip_stats_json(engine.engine())) {
        Ok(text) => text.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeFree(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle == 0 {
        return;
    }
    // Routed through the C entry point rather than dropping the box here.
    //
    // The Kotlin side zeroes its copy before calling, so a second free cannot
    // reach this with the same pointer — but that guard is on the *handle*, and
    // it says nothing about a `step` that is already inside the engine on
    // another thread. That is the race that was actually killing the app: the
    // run loop is a coroutine on a multi-threaded pool, `Job.cancel()` does not
    // wait for it, and the teardown that follows went straight to this function
    // while `TcpRelay::service` was still calling the protector. The crash
    // landed on `BoxedProtector::protect+7` with the fault address one small
    // offset into a box that had already been dropped.
    //
    // `watt_engine_free` is where the in-flight guard lives, and it is the same
    // guard the C ABI gets. Keeping one implementation means the two surfaces
    // cannot drift apart on the one thing they must agree about.
    unsafe { crate::watt_engine_free(handle as *mut crate::WattEngine) };
}

#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeLastError(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let message = crate::last_error().unwrap_or_default();
    match env.new_string(message) {
        Ok(text) => text.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// A raw handle back into a reference, with a claim that keeps it alive.
///
/// Null is the only invalid input; the Kotlin side never produces it. The second
/// way to get `None` is the interesting one: the engine may be in the middle of
/// being freed. That is not hypothetical — the run loop and the teardown are on
/// different threads and nothing orders them — so every entry point that takes a
/// handle has to answer a refusal rather than dereference it.
///
/// The returned guard releases the claim when it drops, which is what lets
/// `free` know it is safe to proceed.
unsafe fn claim_engine(
    handle: jlong,
) -> Option<crate::EngineClaim<'static>> {
    if handle == 0 {
        return None;
    }
    // Safe: the handle is either a live pointer from `nativeNew` or a
    // dereference that the shell's atomic already prevented. The claim is taken
    // through a shared reference because reading the guard field is the only
    // thing that happens before the claim is granted.
    let engine: &'static crate::WattEngine = &*(handle as *const crate::WattEngine);
    crate::EngineClaim::acquire(engine)
}

/// Merge two rule documents. Returns null on failure.
///
/// The merge lives in Rust rather than Kotlin on purpose: it is per *domain*, not
/// per entry, because the compiler keeps only the first entry that claims a
/// domain. Keying on the entry would discard the addresses the merge exists to
/// keep, and that is a rule that is easy to get subtly wrong twice.
///
/// `second_is_hosts` says how to parse the second document. In the app's default
/// setup the second input is the built-in set, and both are hosts/JSON as their
/// caller built them; `/1` is fetched in hosts form (its `?format=json` is a
/// schema the kernel cannot parse), and `/2` is *not* a default because its 862
/// addresses are all `127.0.0.1` and this kernel does not relay loopback. The
/// flag is kept because the first input may legitimately be a JSON document, and
/// parsing a hosts file as JSON fails on the first byte. It is a parameter rather
/// than a guess because guessing wrong produces a confusing parse error rather
/// than an obviously wrong answer.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeMerge(
    mut env: JNIEnv,
    _class: JClass,
    first: JByteArray,
    second: JByteArray,
    second_is_hosts: jni::sys::jboolean,
) -> jni::sys::jbyteArray {
    // Reached before any engine exists: the shell merges the two documents at
    // startup, so a logger installed only in `nativeNew` would miss it.
    init_logcat();

    let a = match env.convert_byte_array(&first) {
        Ok(bytes) => bytes,
        Err(err) => {
            set_error(&mut env, &format!("reading the first document: {err}"));
            return std::ptr::null_mut();
        }
    };
    let b = match env.convert_byte_array(&second) {
        Ok(bytes) => bytes,
        Err(err) => {
            set_error(&mut env, &format!("reading the second document: {err}"));
            return std::ptr::null_mut();
        }
    };
    let second_is_hosts = second_is_hosts != 0;

    let merged = (|| -> Result<Vec<u8>, String> {
        let first = watt_rules::parse_document(&a).map_err(|err| format!("the first document: {err}"))?;
        let second = if second_is_hosts {
            let text = std::str::from_utf8(&b)
                .map_err(|err| format!("the hosts document is not UTF-8: {err}"))?;
            watt_rules::parse_hosts(text)
                .map(|(document, _stats)| document)
                .map_err(|err| format!("the hosts document: {err}"))?
        } else {
            watt_rules::parse_document(&b).map_err(|err| format!("the second document: {err}"))?
        };
        let merged = watt_rules::merge_documents(&[first, second]);
        serde_json::to_vec(&merged).map_err(|err| format!("re-encoding the merge: {err}"))
    })();

    match merged {
        Ok(bytes) => match env.byte_array_from_slice(&bytes) {
            Ok(array) => array.into_raw(),
            Err(err) => {
                set_error(&mut env, &format!("returning the merge: {err}"));
                std::ptr::null_mut()
            }
        },
        Err(message) => {
            set_error(&mut env, &message);
            std::ptr::null_mut()
        }
    }
}

/// Start the local proxy. Returns the handle, or 0 on failure.
///
/// The port is returned by [`Java_dev_detour_core_Kernel_nativeProxyPort`] rather
/// than by this call: passing 0 asks the kernel to choose, and the caller has no
/// other way to learn what it chose.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeProxyStart(
    mut env: JNIEnv,
    _class: JClass,
    port: jint,
    rules: JByteArray,
) -> jlong {
    // The proxy path does not go through `nativeNew`, so the logger has to be
    // installed here too or proxy mode reports nothing at all.
    init_logcat();

    let document = match env.convert_byte_array(&rules) {
        Ok(document) => document,
        Err(err) => {
            set_error(&mut env, &format!("reading the rule document: {err}"));
            return 0;
        }
    };

    let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Safe here: `document` is an owned `Vec<u8>` that outlives the call, and
        // its pointer and length come from the same slice.
        unsafe { crate::watt_proxy_start(port, document.as_ptr(), document.len()) }
    }));

    match started {
        Ok(handle) if !handle.is_null() => handle as jlong,
        Ok(_) => {
            set_error(
                &mut env,
                &crate::last_error().unwrap_or_else(|| "the proxy did not start".to_string()),
            );
            0
        }
        Err(_) => {
            set_error(&mut env, "the proxy constructor panicked");
            0
        }
    }
}

/// The port the proxy actually bound. 0 when the handle is invalid.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeProxyPort(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    if handle == 0 {
        return 0;
    }
    unsafe { crate::watt_proxy_port(handle as *mut crate::WattProxy) }
}

/// Stop the proxy and release the handle. Blocks until the accept loop notices.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeProxyStop(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle == 0 {
        return;
    }
    unsafe { crate::watt_proxy_stop(handle as *mut crate::WattProxy) }
}

/// Swap the rule set of a running proxy. Returns 0 on success, -1 on failure.
///
/// Separate from [`Java_dev_detour_core_Kernel_nativeReplaceRules`] because the
/// two modes hold different state: the tunnel's rules live on the engine, the
/// proxy's on a router behind a lock. Passing a proxy handle to the engine
/// function would reinterpret one struct as the other.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeProxyReplaceRules(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    rules: JByteArray,
) -> jint {
    if handle == 0 {
        return -1;
    }
    let document = match env.convert_byte_array(&rules) {
        Ok(document) => document,
        Err(err) => {
            log::warn!("watt: reading the proxy rule document failed: {err}");
            return -1;
        }
    };
    unsafe {
        crate::watt_proxy_replace_rules(
            handle as *mut crate::WattProxy,
            document.as_ptr(),
            document.len(),
        )
    }
}

/// Start the root-mode reverse proxy. Returns the handle, or 0 on failure.
///
/// `ca_dir` is the app-private directory the CA certificate and key live in. It
/// is a JVM string rather than a byte array because it is a path, and the C ABI
/// wants it NUL-terminated — the JVM's copy is not, so a `CString` is built
/// around it before the call.
///
/// The port is returned by [`Java_dev_detour_core_Kernel_nativeMitmPort`] rather
/// than by this call, for the same reason the plain proxy does it: passing 0
/// asks the kernel to choose, and the caller has no other way to learn what it
/// chose.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeMitmStart(
    mut env: JNIEnv,
    _class: JClass,
    port: jint,
    rules: JByteArray,
    ca_dir: JString,
) -> jlong {
    // The MITM path does not go through `nativeNew`, so the logger has to be
    // installed here too or this mode reports nothing at all.
    init_logcat();

    let document = match env.convert_byte_array(&rules) {
        Ok(document) => document,
        Err(err) => {
            set_error(&mut env, &format!("reading the rule document: {err}"));
            return 0;
        }
    };
    if ca_dir.is_null() {
        set_error(&mut env, "no CA directory was given");
        return 0;
    }
    let directory: String = match env.get_string(&ca_dir) {
        Ok(text) => text.into(),
        Err(err) => {
            set_error(&mut env, &format!("reading the CA directory: {err}"));
            return 0;
        }
    };
    let directory = match CString::new(directory) {
        Ok(text) => text,
        Err(_) => {
            set_error(&mut env, "the CA directory contains a NUL byte");
            return 0;
        }
    };

    let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Safe here: `document` is an owned `Vec<u8>` and `directory` an owned
        // `CString`, both alive for the call, and their pointers come from the
        // values themselves.
        unsafe {
            crate::watt_mitm_start(port, document.as_ptr(), document.len(), directory.as_ptr())
        }
    }));

    match started {
        Ok(handle) if !handle.is_null() => handle as jlong,
        Ok(_) => {
            set_error(
                &mut env,
                &crate::last_error().unwrap_or_else(|| "the mitm proxy did not start".to_string()),
            );
            0
        }
        Err(_) => {
            set_error(&mut env, "the mitm proxy constructor panicked");
            0
        }
    }
}

/// The port the MITM proxy actually bound. 0 when the handle is invalid.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeMitmPort(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    if handle == 0 {
        return 0;
    }
    unsafe { crate::watt_mitm_port(handle as *mut crate::WattMitm) }
}

/// Swap the rule set of a running MITM proxy. Returns 0 on success, -1 on
/// failure.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeMitmReplaceRules(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    rules: JByteArray,
) -> jint {
    if handle == 0 {
        return -1;
    }
    let document = match env.convert_byte_array(&rules) {
        Ok(document) => document,
        Err(err) => {
            log::warn!("watt: reading the mitm rule document failed: {err}");
            return -1;
        }
    };
    unsafe {
        crate::watt_mitm_replace_rules(
            handle as *mut crate::WattMitm,
            document.as_ptr(),
            document.len(),
        )
    }
}

/// The CA certificate in PEM, for the root helper to install. Null on failure.
///
/// Returned as a `jstring` rather than through the C ABI's `char *` so there is
/// no buffer for the shell to free: the JVM copies the bytes when the string is
/// built, and the C buffer is released here before returning. The C entry point
/// (`watt_mitm_ca_pem`) keeps the `watt_free_buffer` contract for non-Java
/// callers; both read the same authority, so the two surfaces cannot disagree.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeMitmCaPem(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    if handle == 0 {
        return std::ptr::null_mut();
    }
    let pointer = unsafe { crate::watt_mitm_ca_pem(handle as *mut crate::WattMitm) };
    if pointer.is_null() {
        return std::ptr::null_mut();
    }
    // Copy out of the C buffer, then release it. The buffer is the boxed-slice
    // form the C ABI hands out, so it is freed with `watt_free_buffer` and a
    // length that includes the NUL terminator — `CStr` stops short of it, hence
    // the `+ 1`.
    let text = unsafe { std::ffi::CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned();
    unsafe { crate::watt_free_buffer(pointer as *mut u8, text.len() + 1) };
    match env.new_string(text) {
        Ok(text) => text.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Fill `out` with the root-mode proxy's counters, in the order
/// `Kernel.MitmStats` reads them.
///
/// Seven fields, fixed by `Kernel.MITM_STATS_FIELDS`. The hazard is the same one
/// `nativeStats` carries: a mismatch does not fail, it silently shifts every
/// field after the gap.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeMitmStats(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    out: JLongArray,
) -> jint {
    if handle == 0 {
        set_error(&mut env, "the mitm proxy is not running");
        return -1;
    }
    let stats = crate::collect_mitm_stats(unsafe { &(*(handle as *mut crate::WattMitm)).proxy });
    let values = [
        stats.connections,
        stats.handshakes,
        stats.served,
        stats.refused,
        stats.dial_failures,
        stats.bytes_to_upstream,
        stats.bytes_to_client,
    ];
    let widened: Vec<jlong> = values.iter().map(|v| *v as jlong).collect();
    if env.set_long_array_region(&out, 0, &widened).is_err() {
        set_error(&mut env, "could not write the counters");
        return -1;
    }
    0
}

/// Stop the MITM proxy and release the handle. Blocks until the accept loop
/// notices.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeMitmStop(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle == 0 {
        return;
    }
    unsafe { crate::watt_mitm_stop(handle as *mut crate::WattMitm) }
}

/// Apply the user's switches to a rule document. Returns null on failure.
///
/// `disabled` is the flat key set the rules screen stores — `g:<group>`,
/// `d:<domain>`, `a:<address>`. The prefixes are parsed by the rule crate, so
/// there is one definition of what a key looks like rather than two that have to
/// be kept in step.
///
/// The filter lives in Rust because the alternative is a second implementation of
/// "which domains does this entry claim" in Kotlin, and the failure mode of the
/// two drifting is silent: a domain quietly stops being routed, or quietly keeps
/// being routed after the user switched it off.
#[no_mangle]
pub extern "system" fn Java_dev_detour_core_Kernel_nativeFilter(
    mut env: JNIEnv,
    _class: JClass,
    rules: JByteArray,
    disabled: jni::objects::JObjectArray,
) -> jni::sys::jbyteArray {
    init_logcat();

    let document = match env.convert_byte_array(&rules) {
        Ok(document) => document,
        Err(err) => {
            set_error(&mut env, &format!("reading the rule document: {err}"));
            return std::ptr::null_mut();
        }
    };

    let keys = match read_strings(&mut env, &disabled) {
        Ok(keys) => keys,
        Err(err) => {
            set_error(&mut env, &format!("reading the switches: {err}"));
            return std::ptr::null_mut();
        }
    };

    let filtered = (|| -> Result<Vec<u8>, String> {
        let parsed =
            watt_rules::parse_document(&document).map_err(|err| format!("the document: {err}"))?;
        let policy = watt_rules::FilterPolicy::from_keys(keys);
        let (result, _stats) = watt_rules::filter_document(&parsed, &policy);
        serde_json::to_vec(&result).map_err(|err| format!("re-encoding: {err}"))
    })();

    match filtered {
        Ok(bytes) => match env.byte_array_from_slice(&bytes) {
            Ok(array) => array.into_raw(),
            Err(err) => {
                set_error(&mut env, &format!("returning the result: {err}"));
                std::ptr::null_mut()
            }
        },
        Err(message) => {
            set_error(&mut env, &message);
            std::ptr::null_mut()
        }
    }
}

/// Read a Java `String[]` into owned Rust strings.
///
/// A null element is skipped rather than failing the call: one malformed entry in
/// a settings list should not stop the tunnel from starting.
fn read_strings(env: &mut JNIEnv, array: &jni::objects::JObjectArray) -> Result<Vec<String>, String> {
    if array.is_null() {
        return Ok(Vec::new());
    }
    let len = env
        .get_array_length(array)
        .map_err(|err| err.to_string())?;
    let mut out = Vec::with_capacity(len as usize);
    for index in 0..len {
        let element = env
            .get_object_array_element(array, index)
            .map_err(|err| err.to_string())?;
        if element.is_null() {
            continue;
        }
        let text: String = env
            .get_string(&element.into())
            .map_err(|err| err.to_string())?
            .into();
        out.push(text);
    }
    Ok(out)
}

fn set_error(env: &mut JNIEnv, message: &str) {
    crate::set_last_error(message);
    // Throwing would be the other option, and a worse one: the Kotlin wrappers
    // already check return values, and an exception raised from a native frame
    // has to be caught in the right place or it aborts the process.
    let _ = env.throw_new(
        "dev/detour/core/KernelException",
        message.replace('\0', " "),
    );
}
