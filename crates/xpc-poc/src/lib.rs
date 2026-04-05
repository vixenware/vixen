use std::ffi::{CStr, CString, c_void};
use std::fs::create_dir_all;
use std::path::PathBuf;
use std::ptr;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, channel};

use block::{Block, ConcreteBlock};
use objc2::rc::Retained;
use objc2_fs_kit::FSUnaryFileSystem;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use xpc_connection_sys::{
    _xpc_error_connection_interrupted, _xpc_error_connection_invalid,
    _xpc_error_termination_imminent, _xpc_type_connection, _xpc_type_dictionary, _xpc_type_error,
    XPC_CONNECTION_MACH_SERVICE_LISTENER, dispatch_main, dispatch_queue_create,
    xpc_connection_activate, xpc_connection_create, xpc_connection_create_from_endpoint,
    xpc_connection_create_mach_service, xpc_connection_get_name,
    xpc_connection_send_message_with_reply_sync, xpc_connection_set_event_handler,
    xpc_connection_t, xpc_copy_description, xpc_dictionary_create_empty,
    xpc_dictionary_create_reply, xpc_dictionary_get_string, xpc_dictionary_set_string,
    xpc_endpoint_create, xpc_get_type, xpc_object_t, xpc_release, xpc_retain,
};

pub mod fskit_poc;

pub const PING_KEY: &str = "ping";
pub const HOUR_KEY: &str = "hour";
pub const PING_VALUE: &str = "what-time-is-it";
pub const REQUEST_KEY: &str = "request";
pub const REQUEST_READ_FILE: &str = "read-file";
pub const REQUEST_FILE_KEY: &str = "file";
pub const RESPONSE_KEY: &str = "contents";
pub const STATUS_KEY: &str = "status";
pub const STATUS_OK: &str = "ok";
pub const STATUS_ERR: &str = "err";
pub const ERROR_KEY: &str = "error";
const APP_GROUP_ID: &str = "B2N6FSRTPV.vixenfs";
const LOG_FILE_NAME: &str = "vixenfs-extension.log";
static TRACING_INIT: OnceLock<()> = OnceLock::new();
static TRACING_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

fn log_file_path() -> PathBuf {
    if let Ok(path) = std::env::var("VIXEN_FS_LOG_PATH") {
        return PathBuf::from(path);
    }
    if let Ok(path) = std::env::var("VIXEN_XPC_LOG_FILE") {
        return PathBuf::from(path);
    }
    real_home_dir()
        .join("Library")
        .join("Group Containers")
        .join(APP_GROUP_ID)
        .join("Logs")
        .join(LOG_FILE_NAME)
}

fn log_destination() -> (PathBuf, String) {
    let log_path = log_file_path();
    let directory = log_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let file_name = log_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(LOG_FILE_NAME)
        .to_owned();
    (directory, file_name)
}

fn real_home_dir() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    let passwd = unsafe { libc::getpwuid(uid) };
    if !passwd.is_null() {
        let dir = unsafe { CStr::from_ptr((*passwd).pw_dir) };
        return PathBuf::from(dir.to_string_lossy().into_owned());
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    if let Some(prefix) = home.split("/Library/Containers/").next() {
        if prefix.starts_with("/Users/") {
            return PathBuf::from(prefix);
        }
    }
    PathBuf::from(home)
}

pub fn log_event(level: &str, message: impl AsRef<str>) {
    let message = message.as_ref();
    match level {
        "trace" => tracing::trace!("{message}"),
        "debug" => tracing::debug!("{message}"),
        "info" => tracing::info!("{message}"),
        "warn" => tracing::warn!("{message}"),
        "error" => tracing::error!("{message}"),
        other => tracing::info!(target: "xpc_poc::legacy_log", level = other, "{message}"),
    }
}

pub fn init_logging() {
    let _ = TRACING_INIT.get_or_init(|| {
        let filter = EnvFilter::new("trace");
        let (log_dir, file_name) = log_destination();
        let _ = create_dir_all(&log_dir);
        let file_appender = tracing_appender::rolling::hourly(log_dir, file_name);
        let (writer, guard) = tracing_appender::non_blocking(file_appender);
        let _ = TRACING_GUARD.set(guard);
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(writer)
            .with_target(true)
            .with_file(true)
            .with_line_number(true);

        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .init();
    });
    log_event("info", "xpc-poc tracing initialized");
}

#[unsafe(no_mangle)]
pub extern "C" fn VixenRustCreateFileSystem() -> *mut c_void {
    init_logging();
    let file_system: Retained<FSUnaryFileSystem> = fskit_poc::create_file_system();
    log_event("info", "created Rust FSKit filesystem object");
    Retained::into_raw(file_system).cast()
}

pub fn log_process_context(args: &[String]) {
    let exe = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("<current_exe error: {err}>"));
    let cwd = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("<current_dir error: {err}>"));
    let pid = std::process::id();
    let ppid = unsafe { libc::getppid() };
    let service_env = std::env::var("VIXEN_XPC_MACH_SERVICE").ok();
    let xpc_service_name = std::env::var("XPC_SERVICE_NAME").ok();
    let xpc_service_bundle = std::env::var("XPC_SERVICE_BUNDLE_IDENTIFIER").ok();
    let launchd_job_label = std::env::var("LAUNCH_JOB_LABEL").ok();

    log_event(
        "info",
        format!("process context pid={pid} ppid={ppid} exe={exe} cwd={cwd} args={args:?}"),
    );
    log_event(
        "info",
        format!(
            "process env VIXEN_XPC_MACH_SERVICE={service_env:?} XPC_SERVICE_NAME={xpc_service_name:?} XPC_SERVICE_BUNDLE_IDENTIFIER={xpc_service_bundle:?} LAUNCH_JOB_LABEL={launchd_job_label:?}"
        ),
    );
}

fn cstring(value: &str) -> CString {
    CString::new(value).expect("CString without interior NULs")
}

fn new_queue(label: &str) -> *mut xpc_connection_sys::dispatch_queue_s {
    let label = cstring(label);
    let queue = unsafe { dispatch_queue_create(label.as_ptr(), ptr::null_mut()) };
    log_event(
        "debug",
        format!(
            "created dispatch queue label={value:?} ptr={queue:?}",
            value = label
        ),
    );
    queue
}

fn xpc_type_name(object: xpc_object_t) -> &'static str {
    let ty = unsafe { xpc_get_type(object) };
    if ty == unsafe { &_xpc_type_connection } {
        "connection"
    } else if ty == unsafe { &_xpc_type_dictionary } {
        "dictionary"
    } else if ty == unsafe { &_xpc_type_error } {
        "error"
    } else {
        "other"
    }
}

fn xpc_description(object: xpc_object_t) -> String {
    let desc = unsafe { xpc_copy_description(object) };
    if desc.is_null() {
        return "<null>".to_owned();
    }
    let string = unsafe { CStr::from_ptr(desc) }
        .to_string_lossy()
        .into_owned();
    unsafe { libc::free(desc.cast()) };
    string
}

fn connection_name(connection: xpc_connection_t) -> Option<String> {
    let name = unsafe { xpc_connection_get_name(connection) };
    if name.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

fn current_hour_string() -> String {
    let mut now = 0;
    let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
    unsafe {
        libc::time(&mut now);
        libc::localtime_r(&now, &mut tm);
    }
    format!("{:02}", tm.tm_hour)
}

fn dictionary_get_string(message: xpc_object_t, key: &str) -> Option<String> {
    let key = cstring(key);
    let value = unsafe { xpc_dictionary_get_string(message, key.as_ptr()) };
    if value.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned(),
    )
}

unsafe fn retain_connection(connection: xpc_connection_t) -> xpc_connection_t {
    unsafe { xpc_retain(connection.cast()) };
    connection
}

fn install_peer_handler(connection: xpc_connection_t, tx: std::sync::mpsc::Sender<String>) {
    log_event(
        "debug",
        format!(
            "installing peer handler for connection raw ptr={:?}",
            connection as *const ()
        ),
    );
    unsafe {
        let retained = retain_connection(connection);
        let peer_name = connection_name(retained).unwrap_or_else(|| "<anonymous-peer>".to_owned());
        log_event(
            "debug",
            format!("activated peer connection peer={peer_name}"),
        );
        let block = ConcreteBlock::new(move |event: xpc_object_t| {
            let ty = xpc_get_type(event);
            if ty == &_xpc_type_dictionary {
                let _ = tx.send(format!(
                    "peer name={peer_name} event={} desc={}",
                    xpc_type_name(event),
                    xpc_description(event)
                ));
                log_event(
                    "debug",
                    format!(
                        "peer event type=dict peer={peer_name} desc={}",
                        xpc_description(event)
                    ),
                );
                let request = dictionary_get_string(event, REQUEST_KEY);
                if dictionary_get_string(event, PING_KEY).as_deref() == Some(PING_VALUE)
                    && request.is_none()
                {
                    log_event(
                        "info",
                        format!("handling ping request for peer={peer_name}"),
                    );
                    let reply = xpc_dictionary_create_reply(event);
                    if reply.is_null() {
                        log_event(
                            "error",
                            format!("failed to create ping reply peer={peer_name}"),
                        );
                        return;
                    }
                    let hour_key = cstring(HOUR_KEY);
                    let hour_value = cstring(&current_hour_string());
                    xpc_dictionary_set_string(reply, hour_key.as_ptr(), hour_value.as_ptr());
                    log_event(
                        "debug",
                        format!("responding to ping hour={}", current_hour_string()),
                    );
                    xpc_connection_sys::xpc_connection_send_message(retained, reply);
                    xpc_release(reply);
                    return;
                }

                if request.as_deref() == Some(REQUEST_READ_FILE)
                    && dictionary_get_string(event, REQUEST_FILE_KEY).as_deref()
                        == Some("/hour.txt")
                {
                    let file = "/hour.txt";
                    let reply = xpc_dictionary_create_reply(event);
                    if reply.is_null() {
                        log_event(
                            "error",
                            format!(
                                "failed to create file reply for peer={peer_name} request_file={}",
                                file
                            ),
                        );
                        return;
                    }
                    let status_key = cstring(STATUS_KEY);
                    let status_ok = cstring(STATUS_OK);
                    let response_key = cstring(RESPONSE_KEY);
                    let response_value = cstring(&format!("{}\n", current_hour_string()));

                    xpc_dictionary_set_string(reply, status_key.as_ptr(), status_ok.as_ptr());
                    xpc_dictionary_set_string(
                        reply,
                        response_key.as_ptr(),
                        response_value.as_ptr(),
                    );
                    xpc_connection_sys::xpc_connection_send_message(retained, reply);
                    xpc_release(reply);
                    log_event(
                        "info",
                        format!("served read-file request for {file} peer={peer_name}"),
                    );
                    return;
                }

                let reply = if request.is_some() {
                    log_event(
                        "warn",
                        format!("unsupported request from peer={peer_name} request={request:?}"),
                    );
                    let reply = xpc_dictionary_create_reply(event);
                    if reply.is_null() {
                        return;
                    }
                    let status_key = cstring(STATUS_KEY);
                    let status_err = cstring(STATUS_ERR);
                    let error_key = cstring(ERROR_KEY);
                    let error_message = cstring("unsupported request");

                    xpc_dictionary_set_string(reply, status_key.as_ptr(), status_err.as_ptr());
                    xpc_dictionary_set_string(reply, error_key.as_ptr(), error_message.as_ptr());
                    reply
                } else {
                    xpc_dictionary_create_reply(event)
                };
                if reply.is_null() {
                    return;
                }
                if request.is_none() {
                    log_event("warn", format!("invalid payload from peer={peer_name}"));
                    let status_key = cstring(STATUS_KEY);
                    let status_err = cstring(STATUS_ERR);
                    let error_key = cstring(ERROR_KEY);
                    let error_message = cstring("invalid payload");

                    xpc_dictionary_set_string(reply, status_key.as_ptr(), status_err.as_ptr());
                    xpc_dictionary_set_string(reply, error_key.as_ptr(), error_message.as_ptr());
                }
                xpc_connection_sys::xpc_connection_send_message(retained, reply);
                xpc_release(reply);
            } else if ty == &_xpc_type_error {
                let interrupted: xpc_object_t = ptr::addr_of!(_xpc_error_connection_interrupted)
                    .cast_mut()
                    .cast();
                let invalid: xpc_object_t = ptr::addr_of!(_xpc_error_connection_invalid)
                    .cast_mut()
                    .cast();
                let imminent: xpc_object_t = ptr::addr_of!(_xpc_error_termination_imminent)
                    .cast_mut()
                    .cast();
                let _ = tx.send(format!(
                    "peer name={peer_name} error desc={}",
                    xpc_description(event)
                ));
                log_event(
                    "warn",
                    format!(
                        "peer error peer={peer_name} event={}",
                        xpc_description(event)
                    ),
                );
                if event == interrupted {
                    return;
                }
                if event == invalid {
                    return;
                }
                if event == imminent {
                    return;
                }
            }
        })
        .copy();
        xpc_connection_set_event_handler(
            retained,
            (&*block as *const Block<_, _>).cast_mut().cast(),
        );
        xpc_connection_activate(retained);
    }
}

fn install_listener_handler(listener: xpc_connection_t, tx: std::sync::mpsc::Sender<String>) {
    log_event(
        "info",
        format!("starting listener handler listener={listener:?}"),
    );
    unsafe {
        let block = ConcreteBlock::new(move |event: xpc_object_t| {
            let ty = xpc_get_type(event);
            if ty == &_xpc_type_connection {
                let peer = event.cast();
                let peer_name =
                    connection_name(peer).unwrap_or_else(|| "<anonymous-peer>".to_owned());
                let _ = tx.send(format!("accepted connection peer={peer_name}"));
                log_event("info", format!("accepted peer={peer_name}"));
                install_peer_handler(peer, tx.clone());
            } else {
                log_event(
                    "debug",
                    format!(
                        "listener event type={} desc={}",
                        xpc_type_name(event),
                        xpc_description(event)
                    ),
                );
                let _ = tx.send(format!(
                    "listener event type={} desc={}",
                    xpc_type_name(event),
                    xpc_description(event)
                ));
            }
        })
        .copy();
        xpc_connection_set_event_handler(
            listener,
            (&*block as *const Block<_, _>).cast_mut().cast(),
        );
        xpc_connection_activate(listener);
        log_event("info", format!("activated listener listener={listener:?}"));
    }
}

pub fn start_anonymous_hour_service() -> (xpc_connection_t, Receiver<String>) {
    log_event("info", "creating anonymous xpc listener");
    let listener_queue = new_queue("eu.bearcove.vixen.xpc-poc.listener");
    let listener = unsafe { xpc_connection_create(ptr::null(), listener_queue) };
    log_event(
        "info",
        format!("anonymous listener created queue={listener_queue:?} listener={listener:?}"),
    );
    let (tx, rx) = channel();
    install_listener_handler(listener, tx);

    (listener, rx)
}

pub fn start_mach_hour_service(service_name: &str) -> (xpc_connection_t, Receiver<String>) {
    log_event(
        "info",
        format!("creating mach listener service_name={service_name}"),
    );
    let listener_queue = new_queue("eu.bearcove.vixen.xpc-poc.mach-listener");
    let service_name = cstring(service_name);
    let listener = unsafe {
        xpc_connection_create_mach_service(
            service_name.as_ptr(),
            listener_queue,
            u64::from(XPC_CONNECTION_MACH_SERVICE_LISTENER),
        )
    };
    log_event(
        "info",
        format!(
            "mach listener created queue={listener_queue:?} listener={listener:?} service_name={}",
            service_name.to_string_lossy()
        ),
    );
    let (tx, rx) = channel();
    install_listener_handler(listener, tx);
    (listener, rx)
}

pub fn request_hour(listener: xpc_connection_t) -> String {
    log_event("info", "request_hour start");
    let endpoint = unsafe { xpc_endpoint_create(listener) };
    assert!(
        !endpoint.is_null(),
        "expected endpoint from anonymous listener"
    );

    let client = unsafe { xpc_connection_create_from_endpoint(endpoint) };
    assert!(!client.is_null(), "expected client from endpoint");

    unsafe {
        let block = ConcreteBlock::new(|_event: xpc_object_t| {}).copy();
        xpc_connection_set_event_handler(client, (&*block as *const Block<_, _>).cast_mut().cast());
        xpc_connection_activate(client);
    }

    let request = unsafe { xpc_dictionary_create_empty() };
    let ping_key = cstring(PING_KEY);
    let ping_value = cstring(PING_VALUE);
    unsafe {
        xpc_dictionary_set_string(request, ping_key.as_ptr(), ping_value.as_ptr());
    }

    let reply = unsafe { xpc_connection_send_message_with_reply_sync(client, request) };
    assert!(!reply.is_null(), "expected synchronous reply");
    let ty = unsafe { xpc_get_type(reply) };
    assert_eq!(
        ty,
        unsafe { &_xpc_type_dictionary },
        "expected dictionary reply"
    );

    let hour_key = cstring(HOUR_KEY);
    let hour = unsafe { xpc_dictionary_get_string(reply, hour_key.as_ptr()) };
    assert!(!hour.is_null(), "expected hour field in reply");
    let hour = unsafe { CStr::from_ptr(hour) }
        .to_string_lossy()
        .into_owned();
    log_event("debug", format!("request_hour response={hour}"));

    unsafe {
        xpc_release(reply);
        xpc_release(request);
        xpc_release(client.cast());
        xpc_release(endpoint.cast());
    }

    hour
}

pub fn request_read_file(listener: xpc_connection_t, file: &str) -> String {
    log_event("info", format!("request_read_file start file={file}"));
    let endpoint = unsafe { xpc_endpoint_create(listener) };
    assert!(
        !endpoint.is_null(),
        "expected endpoint from anonymous listener"
    );

    let client = unsafe { xpc_connection_create_from_endpoint(endpoint) };
    assert!(!client.is_null(), "expected client from endpoint");

    unsafe {
        let block = ConcreteBlock::new(|_event: xpc_object_t| {}).copy();
        xpc_connection_set_event_handler(client, (&*block as *const Block<_, _>).cast_mut().cast());
        xpc_connection_activate(client);
    }

    let request = unsafe { xpc_dictionary_create_empty() };
    let request_key = cstring(REQUEST_KEY);
    let operation = cstring(REQUEST_READ_FILE);
    let file_key = cstring(REQUEST_FILE_KEY);
    let file = cstring(file);
    unsafe {
        xpc_dictionary_set_string(request, request_key.as_ptr(), operation.as_ptr());
        xpc_dictionary_set_string(request, file_key.as_ptr(), file.as_ptr());
    }

    let reply = unsafe { xpc_connection_send_message_with_reply_sync(client, request) };
    assert!(!reply.is_null(), "expected synchronous reply");
    let ty = unsafe { xpc_get_type(reply) };
    assert_eq!(
        ty,
        unsafe { &_xpc_type_dictionary },
        "expected dictionary reply"
    );

    let status = dictionary_get_string(reply, STATUS_KEY).unwrap_or_else(|| STATUS_ERR.to_owned());
    if status != STATUS_OK {
        let error =
            dictionary_get_string(reply, ERROR_KEY).unwrap_or_else(|| "unknown error".to_owned());
        log_event(
            "error",
            format!("request_read_file failed status={status} error={error}"),
        );
        panic!("request failed: {error}");
    }

    let contents_key = cstring(RESPONSE_KEY);
    let contents = unsafe { xpc_dictionary_get_string(reply, contents_key.as_ptr()) };
    assert!(!contents.is_null(), "expected file contents in reply");
    let contents = unsafe { CStr::from_ptr(contents) }
        .to_string_lossy()
        .into_owned();
    log_event(
        "debug",
        format!("request_read_file response={} bytes", contents.len()),
    );

    unsafe {
        xpc_release(reply);
        xpc_release(request);
        xpc_release(client.cast());
        xpc_release(endpoint.cast());
    }

    contents
}

pub fn run_dispatch_main() -> ! {
    log_event("info", "entering dispatch_main");
    unsafe { dispatch_main() };
    loop {
        log_event("debug", "dispatch thread parked");
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::{request_hour, start_anonymous_hour_service};

    #[test]
    fn anonymous_listener_round_trips_hour_reply() {
        let (listener, rx) = start_anonymous_hour_service();
        let hour = request_hour(listener);
        let accept = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();

        assert_eq!(accept, "accepted connection peer=<anonymous-peer>");
        assert_eq!(hour.len(), 2);
        assert!(hour.chars().all(|ch| ch.is_ascii_digit()));
    }
}
