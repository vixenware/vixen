use xpc_poc::{
    init_logging, log_event, log_process_context, request_hour, request_read_file,
    run_dispatch_main, start_anonymous_hour_service, start_mach_hour_service,
};

const DEFAULT_EXTENSION_MACH_SERVICE: &str = "eu.bearcove.vixenfs";

fn main() {
    init_logging();
    let args = std::env::args().collect::<Vec<_>>();
    log_process_context(&args);
    log_event("info", format!("starting xpc-poc args={args:?}"));
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some("anonymous-hour"), None) => {
            let (listener, _rx) = start_anonymous_hour_service();
            let hour = request_hour(listener);
            log_event("info", format!("anonymous-hour returned {hour}"));
            println!("hour={hour}");
        }
        (Some("read-hour-file"), Some(path)) => {
            let (listener, _rx) = start_anonymous_hour_service();
            let contents = request_read_file(listener, &path);
            log_event("info", format!("read-hour-file response path={path}"));
            print!("{contents}");
        }
        (Some("mach-service"), Some(name)) => {
            let (_listener, rx) = start_mach_hour_service(&name);
            log_event("info", format!("listening on mach service {name}"));
            std::thread::spawn(move || {
                while let Ok(line) = rx.recv() {
                    log_event("info", format!("xpc event: {line}"));
                }
            });
            run_dispatch_main();
        }
        _ => {
            let service_name = std::env::var("VIXEN_XPC_MACH_SERVICE")
                .unwrap_or_else(|_| DEFAULT_EXTENSION_MACH_SERVICE.to_owned());
            let (_listener, rx) = start_mach_hour_service(&service_name);
            log_event(
                "info",
                format!("defaulting to mach service listener {service_name}"),
            );
            std::thread::spawn(move || {
                while let Ok(line) = rx.recv() {
                    log_event("info", format!("xpc event: {line}"));
                }
            });
            run_dispatch_main();
        }
    }
}
