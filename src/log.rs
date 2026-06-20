use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::sync::Once;

const LOG_PID: c_int = 0x01;
const LOG_NDELAY: c_int = 0x08;
const LOG_MAIL: c_int = 16 << 3;
const LOG_ERR: c_int = 3;
const LOG_INFO: c_int = 6;
const LOG_DEBUG: c_int = 7;

unsafe extern "C" {
    fn openlog(ident: *const c_char, logopt: c_int, facility: c_int);
    fn syslog(priority: c_int, format: *const c_char, ...);
}

static OPEN_SYSLOG: Once = Once::new();

pub struct Logger {
    debug_stderr: bool,
}

impl Logger {
    pub fn new(debug_stderr: bool) -> Self {
        OPEN_SYSLOG.call_once(|| {
            let ident = CString::new("slac").expect("static ident has no nul");
            unsafe {
                openlog(ident.as_ptr(), LOG_PID | LOG_NDELAY, LOG_MAIL);
            }
        });
        Self { debug_stderr }
    }

    pub fn info(&self, message: &str) {
        self.write(LOG_INFO, "info", message);
    }

    pub fn debug(&self, message: &str) {
        self.write(LOG_DEBUG, "debug", message);
    }

    pub fn err(&self, message: &str) {
        self.write(LOG_ERR, "error", message);
    }

    fn write(&self, priority: c_int, level: &str, message: &str) {
        if self.debug_stderr {
            eprintln!("slac[{level}]: {message}");
        }

        let sanitized = message.replace('\0', "\\0");
        let Ok(format) = CString::new("%s") else {
            return;
        };
        let Ok(c_message) = CString::new(sanitized) else {
            return;
        };
        unsafe {
            syslog(priority, format.as_ptr(), c_message.as_ptr());
        }
    }
}
