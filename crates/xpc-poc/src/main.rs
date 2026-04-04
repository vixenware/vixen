use xpc_poc::{
    init_logging, log_event, request_hour, request_read_file, run_dispatch_main,
    start_anonymous_hour_service, start_mach_hour_service,
};

fn main() {
    init_logging();
    log_event("info", "starting xpc-poc");
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
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
            let (listener, _rx) = start_anonymous_hour_service();
            let hour = request_hour(listener);
            log_event("info", format!("default request_hour returned {hour}"));
            println!("hour={hour}");
        }
    }
}
